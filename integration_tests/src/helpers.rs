//   Copyright 2023 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::{
    fmt::{Debug, Display},
    net::{Ipv4Addr, TcpListener},
    ops::ControlFlow,
    time::Duration,
};

use libp2p::{Multiaddr, multiaddr::Protocol};
use tari_engine_types::substate::SubstateId;
use tari_ootle_common_types::SubstateRequirement;
use tokio::{io::AsyncWriteExt, net::TcpStream, task::JoinHandle};

use crate::{TariWorld, wait::wait_until};

/// How long a spawned process has to start listening on its port.
pub const PROCESS_START_TIMEOUT: Duration = Duration::from_secs(40);

pub fn get_os_assigned_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

pub fn get_os_assigned_ports() -> (u16, u16) {
    (get_os_assigned_port(), get_os_assigned_port())
}

/// The libp2p address of a process listening on `port` on the loopback interface.
pub fn local_tcp_multiaddr(port: u16) -> Multiaddr {
    Multiaddr::empty()
        .with(Protocol::Ip4(Ipv4Addr::LOCALHOST))
        .with(Protocol::Tcp(port))
}

/// Waits for the process `name` to accept connections on `port`, returning `false` if `has_exited` reports that it
/// stopped first.
async fn wait_for_local_listener(name: &str, port: u16, has_exited: impl Fn() -> bool) -> bool {
    let result = wait_until(PROCESS_START_TIMEOUT, async || {
        if has_exited() {
            return ControlFlow::Break(false);
        }
        match TcpStream::connect((Ipv4Addr::LOCALHOST, port)).await {
            Ok(mut stream) => {
                stream.shutdown().await.unwrap();
                ControlFlow::Break(true)
            },
            Err(err) => ControlFlow::Continue(err.to_string()),
        }
    })
    .await;
    result.unwrap_or_else(|err| panic!("{name} did not start listening on port {port}: {err}"))
}

pub async fn wait_listener_on_local_port_os_thread<T, E: Debug>(
    name: &'static str,
    handle: std::thread::JoinHandle<Result<T, E>>,
    port: u16,
) -> std::thread::JoinHandle<Result<T, E>> {
    if !wait_for_local_listener(name, port, || handle.is_finished()).await {
        if let Err(err) = handle.join().unwrap_or_else(|_| panic!("{name} panicked")) {
            panic!("{name} exited with error: {err:?}");
        }
        panic!("{name} exited cleanly unexpectedly");
    }
    handle
}

pub async fn wait_listener_on_local_port<T, E: Debug>(
    name: &'static str,
    handle: JoinHandle<Result<T, E>>,
    port: u16,
) -> JoinHandle<Result<T, E>> {
    if !wait_for_local_listener(name, port, || handle.is_finished()).await {
        match handle.await {
            Ok(Ok(_)) => panic!("{name} exited cleanly unexpectedly"),
            Ok(Err(e)) => panic!("{name} exited with error: {:?}", e),
            Err(e) => {
                let panic = e.into_panic();
                panic!(
                    "{name} panicked {:?}",
                    panic
                        .downcast_ref::<&str>()
                        .copied()
                        .or_else(|| panic.downcast_ref::<String>().map(|s| s.as_str()))
                        .unwrap()
                );
            },
        }
    }
    handle
}

pub async fn check_join_handle<E: Display>(
    name: &str,
    handle: tokio::task::JoinHandle<Result<(), E>>,
) -> tokio::task::JoinHandle<Result<(), E>> {
    if !handle.is_finished() {
        return handle;
    }

    match handle.await {
        Ok(Ok(_)) => {
            panic!("Node {} exited unexpectedly", name);
        },
        Ok(Err(e)) => {
            panic!("Node {} exited unexpectedly with error: {}", name, e);
        },
        Err(e) => {
            panic!("Node {} panicked: {:?}", name, e.try_into_panic());
        },
    }
}

pub fn get_address_from_output(world: &TariWorld, output_ref: String) -> &SubstateId {
    world
        .outputs
        .iter()
        .find_map(|(parent_name, outputs)| {
            outputs
                .iter()
                .find(|(child_name, _)| {
                    let fqn = format!("{}/{}", parent_name, child_name);
                    fqn == output_ref
                })
                .map(|(_, addr)| &addr.substate_id)
        })
        .unwrap_or_else(|| panic!("Output not found: {}", output_ref))
}

pub fn get_component_from_namespace(world: &TariWorld, fq_component_name: String) -> SubstateRequirement {
    let (input_group, component_name) = fq_component_name.split_once('/').unwrap_or_else(|| {
        panic!(
            "Component name must be in the format '{{group}}/components/{{template_name}}', got {}",
            fq_component_name
        )
    });

    world
        .outputs
        .get(input_group)
        .unwrap_or_else(|| panic!("No outputs found with name {}", input_group))
        .iter()
        .find(|(name, _)| **name == component_name)
        .map(|(_, data)| data.clone())
        .unwrap_or_else(|| panic!("No component named {}", component_name))
}
