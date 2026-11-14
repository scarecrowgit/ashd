//! The read-only backend through the real service loop over a fixture tree (no
//! NetworkManager, nothing read from the machine).

use std::{fs, time::Duration};

use async_channel::Receiver;
use ashd_svc_network::{Backend, Command, LinkStatus, NetworkState, NmMode, Options, Paths, start_with};

fn wait_for(rx: &Receiver<NetworkState>, what: &str, pred: impl Fn(&NetworkState) -> bool) -> NetworkState {
    smol::block_on(smol::future::or(
        async {
            loop {
                match rx.recv().await {
                    Ok(s) if pred(&s) => return s,
                    Ok(_) => {}
                    Err(_) => panic!("closed waiting for {what}"),
                }
            }
        },
        async {
            smol::Timer::after(Duration::from_secs(10)).await;
            panic!("timed out waiting for: {what}")
        },
    ))
}

fn fixture() -> (tempfile::TempDir, Paths) {
    let dir = tempfile::tempdir().unwrap();
    let p = Paths::under(dir.path());
    let d = p.sys_net.join("eno1");
    fs::create_dir_all(d.join("device")).unwrap();
    fs::create_dir_all(d.join("statistics")).unwrap();
    for (f, c) in [("operstate", "up"), ("carrier", "1"), ("speed", "100"), ("duplex", "full"), ("mtu", "1500"), ("statistics/rx_bytes", "10"), ("statistics/tx_bytes", "5")] {
        fs::write(d.join(f), format!("{c}\n")).unwrap();
    }
    fs::create_dir_all(p.proc_route.parent().unwrap()).unwrap();
    fs::write(&p.proc_route, "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\neno1\t00000000\t0164A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0\n").unwrap();
    (dir, p)
}

fn opts(p: Paths) -> Options {
    Options {
        paths: p,
        addrs: Some(Box::new(|| vec![("eno1".into(), "192.168.100.24".parse().unwrap(), 24)])),
        usage_file: None,
        nm: NmMode::Off,
        fake_commands: false,
        mock: None,
        safe_apply: Duration::from_secs(15),
    }
}

#[test]
fn serves_the_kernel_view_and_refuses_configuration() {
    let (_dir, p) = fixture();
    let (handle, rx) = start_with(opts(p));
    let s = wait_for(&rx, "first state", |s| s.available);
    assert_eq!(s.backend, Backend::ReadOnly);
    assert!(!s.can_configure);
    assert_eq!(s.status(), LinkStatus::Connected);
    assert_eq!(s.tile_subtitle(), "eno1 · 100 Mb/s");
    assert!(!s.sampling);

    // Sampling on: the fast loop fills the history.
    handle.set_sampling(true);
    let s = wait_for(&rx, "history", |s| s.sampling && s.throughput.history.len() >= 2);
    assert!(s.throughput.sampled_at.is_some());
    handle.set_sampling(false);
    wait_for(&rx, "sampling off", |s| !s.sampling);

    // A command that needs NetworkManager changes nothing and says why.
    handle.send(Command::ActivateProfile { uuid: "x".into() });
    let s = wait_for(&rx, "error", |s| s.error.is_some());
    assert_eq!(s.error.as_deref(), Some("needs NetworkManager"));
    assert_eq!(s.backend, Backend::ReadOnly);
}

#[test]
fn mock_scenarios_are_served() {
    let (_dir, p) = fixture();
    let mut o = opts(p);
    o.mock = Some("limited".into());
    let (_handle, rx) = start_with(o);
    let s = wait_for(&rx, "mock", |s| s.available);
    assert_eq!(s.status(), LinkStatus::Limited);
}
