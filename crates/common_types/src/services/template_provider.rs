//  Copyright 2022 The Tari Project
//  SPDX-License-Identifier: BSD-3-Clause

use tari_ootle_template_metadata::MetadataHash;
use tari_template_lib_types::{Hash32, TemplateAddress, crypto::RistrettoPublicKeyBytes};

use crate::Epoch;

pub trait TemplateProvider: Send + Sync + Clone + 'static {
    type Template;
    type Error: std::error::Error + Sync + Send + 'static;

    fn get_template(&self, address: &TemplateAddress) -> Result<Option<Self::Template>, Self::Error>;
    fn has_template(&self, address: &TemplateAddress) -> Result<bool, Self::Error> {
        Ok(self.get_template(address)?.is_some())
    }

    /// Offer a template an execution has just produced, so that a provider able to keep it does not
    /// have to produce it again.
    ///
    /// An offer is a gift, not a request: a provider is free to ignore it, and an implementor that
    /// does is indistinguishable from one that keeps it except in how long the next load takes. The
    /// caller learns nothing either way.
    ///
    /// The template is one the execution derived rather than one this provider served, so it may
    /// belong to a substate that never commits. A provider that keeps offers must therefore treat
    /// what it keeps as an artifact alone and not as evidence that the template exists.
    fn offer_compiled(&self, _address: &TemplateAddress, _template: &Self::Template) {}
}

pub trait TemplateMetadataProvider: TemplateProvider {
    fn get_template_metadata(&self, id: &TemplateAddress) -> Result<Option<TemplateProviderMetadata>, Self::Error>;
}

#[derive(Debug, Clone)]
pub struct TemplateProviderMetadata {
    pub author: RistrettoPublicKeyBytes,
    pub binary_hash: Hash32,
    pub epoch: Epoch,
    pub metadata_hash: Option<MetadataHash>,
}
