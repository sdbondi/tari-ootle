//   Copyright 2023 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use tari_engine_types::ownership::Ownership;
use tari_template_lib::types::{
    NonFungibleAddress,
    SubstateOwnerRule,
    access_rules::{
        AccessRule,
        RequireRule,
        ResourceAccessRules,
        ResourceAuthAction,
        RestrictedAccessRule,
        RuleRequirement,
    },
};

use crate::{
    runtime::{
        ActionIdent,
        AuthorizationScope,
        RuntimeError,
        working_state::{MethodCaller, WorkingState},
    },
    state_store::StateReader,
};

/// The identity an access rule is evaluated against.
#[derive(Clone, Copy)]
enum RuleContext {
    /// Resource, ownership and covenant checks: the current (top) frame is the actor.
    CurrentFrame,
    /// Component method checks: the actor is the frame immediately below the callee. `None` when the
    /// method is invoked directly by a top-level transaction instruction (the signer is the caller).
    Caller(Option<MethodCaller>),
}

pub struct Authorization<'a, TStore> {
    state: &'a WorkingState<TStore>,
}

impl<'a, TStore: StateReader> Authorization<'a, TStore> {
    pub(super) fn new(state: &'a WorkingState<TStore>) -> Self {
        Self { state }
    }

    pub fn check_current_component_access_rules(&self, method: &str) -> Result<(), RuntimeError> {
        let locked = self
            .state
            .current_call_scope()?
            .get_current_component_lock()
            .ok_or_else(|| RuntimeError::InvariantError {
                function: "check_component_access_rules",
                details: "No current component lock in call scope".to_string(),
            })?;
        let component = self.state.get_component(locked)?;
        let scope = self.state.current_call_scope()?.auth_scope();

        // The callee frame is already on top of the stack here, so both the ownership check and the
        // method access rule must be evaluated against the caller (the frame below the callee).
        let context = RuleContext::Caller(self.state.method_caller());
        if check_ownership(self.state, scope, component.as_ownership(), context)? {
            // Owner can call any component method
            return Ok(());
        }

        let component_address =
            locked
                .substate_id()
                .as_component_address()
                .ok_or_else(|| RuntimeError::InvariantError {
                    function: "check_component_access_rules",
                    details: format!("Expected a component address, got {}", locked.substate_id()),
                })?;

        let access_rule = component.access_rules().get_method_access_rule(method);
        if !check_access_rule(self.state, scope, access_rule, context)? {
            return Err(RuntimeError::AccessDenied {
                action_ident: ActionIdent::ComponentCallMethod {
                    component_address,
                    method: method.to_string(),
                },
            });
        }
        Ok(())
    }

    pub fn check_resource_access_rules(
        &self,
        action: ResourceAuthAction,
        resource_ownership: Ownership<'_>,
        resource_access_rules: &ResourceAccessRules,
    ) -> Result<(), RuntimeError> {
        let scope = self.state.current_call_scope()?.auth_scope();

        // Check ownership.
        // A resource is only recallable by explicit access rules
        if !action.is_recall() && check_ownership(self.state, scope, resource_ownership, RuleContext::CurrentFrame)? {
            // Owner can invoke any resource method
            return Ok(());
        }

        let rule = resource_access_rules.get_access_rule(&action);
        if !check_access_rule(self.state, scope, rule, RuleContext::CurrentFrame)? {
            return Err(RuntimeError::AccessDenied {
                action_ident: action.into(),
            });
        }

        Ok(())
    }

    pub fn check_access_rule(&self, rule: &AccessRule) -> Result<bool, RuntimeError> {
        let scope = self.state.current_call_scope()?.auth_scope();
        check_access_rule(self.state, scope, rule, RuleContext::CurrentFrame)
    }

    /// Returns `true` if the current frame satisfies the ownership rule of a non-component substate
    /// (resource, fee pool). The current frame is the actor for these substates.
    pub fn check_ownership_in_current_frame(&self, ownership: Ownership<'_>) -> Result<bool, RuntimeError> {
        let scope = self.state.current_call_scope()?.auth_scope();
        check_ownership(self.state, scope, ownership, RuleContext::CurrentFrame)
    }

    /// Requires that the current frame satisfies the ownership rule of a non-component substate
    /// (resource, fee pool). Component ownership must use
    /// [`require_component_ownership`](Self::require_component_ownership).
    pub fn require_ownership_in_current_frame<A: Into<ActionIdent>>(
        &self,
        action: A,
        ownership: Ownership<'_>,
    ) -> Result<(), RuntimeError> {
        if !self.check_ownership_in_current_frame(ownership)? {
            return Err(RuntimeError::AccessDeniedOwnerRequired { action: action.into() });
        }
        Ok(())
    }

    /// Requires that the caller of the current component satisfies the component's ownership rule. A component
    /// ownership rule is always evaluated against the caller (the frame below the current one), because every
    /// component action runs with the component's own frame already on top of the stack.
    pub fn require_component_ownership<A: Into<ActionIdent>>(
        &self,
        action: A,
        ownership: Ownership<'_>,
    ) -> Result<(), RuntimeError> {
        let context = RuleContext::Caller(self.state.method_caller());
        if !check_ownership(
            self.state,
            self.state.current_call_scope()?.auth_scope(),
            ownership,
            context,
        )? {
            return Err(RuntimeError::AccessDeniedOwnerRequired { action: action.into() });
        }
        Ok(())
    }
}

fn check_ownership<TStore: StateReader>(
    state: &WorkingState<TStore>,
    scope: &AuthorizationScope,
    ownership: Ownership<'_>,
    context: RuleContext,
) -> Result<bool, RuntimeError> {
    match ownership.owner_rule.as_ref() {
        SubstateOwnerRule::None => Ok(false),
        SubstateOwnerRule::ByAccessRule(rule) => check_access_rule(state, scope, rule, context),
        SubstateOwnerRule::ByPublicKey(key) => {
            let owner_proof = NonFungibleAddress::from_public_key(*key);
            Ok(scope.contains_badge(&owner_proof))
        },
    }
}

fn check_access_rule<TStore: StateReader>(
    state: &WorkingState<TStore>,
    scope: &AuthorizationScope,
    rule: &AccessRule,
    context: RuleContext,
) -> Result<bool, RuntimeError> {
    match rule {
        AccessRule::AllowAll => Ok(true),
        AccessRule::DenyAll => Ok(false),
        AccessRule::Restricted(rule) => check_restricted_access_rule(state, scope, rule, context),
    }
}

fn check_restricted_access_rule<TStore: StateReader>(
    state: &WorkingState<TStore>,
    scope: &AuthorizationScope,
    rule: &RestrictedAccessRule,
    context: RuleContext,
) -> Result<bool, RuntimeError> {
    match rule {
        RestrictedAccessRule::Require(rule) => check_require_rule(state, scope, rule, context),
        RestrictedAccessRule::AnyOf(rules) => {
            for rule in rules {
                if check_restricted_access_rule(state, scope, rule, context)? {
                    return Ok(true);
                }
            }
            Ok(false)
        },
        RestrictedAccessRule::AllOf(rules) => {
            // Empty AllOf is denied rather than vacuously true, to prevent accidental AllowAll
            if rules.is_empty() {
                return Ok(false);
            }
            for rule in rules {
                if !check_restricted_access_rule(state, scope, rule, context)? {
                    return Ok(false);
                }
            }
            Ok(true)
        },
    }
}

fn check_require_rule<TStore: StateReader>(
    state: &WorkingState<TStore>,
    scope: &AuthorizationScope,
    rule: &RequireRule,
    context: RuleContext,
) -> Result<bool, RuntimeError> {
    match rule {
        RequireRule::Require(requirement) => check_requirement(state, scope, requirement, context),
        RequireRule::AnyOf(requirements) => {
            for requirement in requirements {
                if check_requirement(state, scope, requirement, context)? {
                    return Ok(true);
                }
            }

            Ok(false)
        },
        RequireRule::AllOf(requirements) => {
            // Empty AllOf is denied rather than vacuously true, to prevent accidental AllowAll
            if requirements.is_empty() {
                return Ok(false);
            }
            for requirement in requirements {
                if !check_requirement(state, scope, requirement, context)? {
                    return Ok(false);
                }
            }

            Ok(true)
        },
        RequireRule::MOfN(n, requirements) => {
            // 0-of-N is vacuously satisfied: no requirements need to be met
            if *n == 0 {
                return Ok(true);
            }
            let mut satisfied = 0u16;
            for requirement in requirements {
                if check_requirement(state, scope, requirement, context)? {
                    satisfied += 1;
                    if satisfied == *n {
                        return Ok(true);
                    }
                }
            }

            Ok(false)
        },
    }
}

fn check_requirement<TStore: StateReader>(
    state: &WorkingState<TStore>,
    scope: &AuthorizationScope,
    requirement: &RuleRequirement,
    context: RuleContext,
) -> Result<bool, RuntimeError> {
    match requirement {
        RuleRequirement::Resource(resx) => {
            if scope.contains_badge_of_resource(resx) {
                return Ok(true);
            }

            for proof_id in scope.proofs() {
                let proof = state.get_proof(*proof_id)?;

                if resx == proof.resource_address() {
                    return Ok(true);
                }
            }
            Ok(false)
        },
        RuleRequirement::NonFungibleAddress(addr) => {
            if scope.contains_badge(addr) {
                return Ok(true);
            }

            for proof_id in scope.proofs() {
                let proof = state.get_proof(*proof_id)?;

                if addr.resource_address() == proof.resource_address() &&
                    proof.non_fungible_token_ids().contains(addr.id())
                {
                    return Ok(true);
                }
            }

            Ok(false)
        },
        // `ScopedToComponent` / `ScopedToTemplate` mean "execution is within this component/template":
        // they always describe the current (top) frame, which is the actor on resource/ownership checks
        // and the callee on method checks.
        RuleRequirement::ScopedToComponent(address) => Ok(state.current_component()? == Some(*address)),
        RuleRequirement::ScopedToTemplate(address) => {
            let current = state.current_template()?;
            Ok(current == address)
        },
        // `CallerComponent` / `DirectCallerTemplate` mean "the caller is this component/template": they are only
        // meaningful on method checks, and a top-level signer has no component/template identity to match.
        RuleRequirement::CallerComponent(address) => match context {
            RuleContext::Caller(Some(caller)) => Ok(caller.component == Some(*address)),
            RuleContext::Caller(None) | RuleContext::CurrentFrame => Ok(false),
        },
        RuleRequirement::DirectCallerTemplate(address) => match context {
            RuleContext::Caller(Some(caller)) => Ok(caller.template == *address),
            RuleContext::Caller(None) | RuleContext::CurrentFrame => Ok(false),
        },
    }
}
