//! Hand-driven check that a real `lunchbox-stated` answers a real `RemoteStore`.
//! Not a test: it needs an installed custodian and a live kiosk session.
use lunchbox_store::Store;
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let user = std::env::args().nth(1).unwrap_or_else(|| "kiosk".into());
    let store = lunchbox_state_proto::RemoteStore::connect(&user)?;
    println!("connected: {}", store.socket_path().display());
    println!("healthy: {}", store.is_healthy());
    let entry = lunchbox_util::EntryId::new("probe-entry");
    let day = lunchbox_util::now().date_naive();
    println!("usage before: {:?}", store.get_usage(&entry, day)?);
    store.add_usage(&entry, day, Duration::from_secs(7))?;
    println!("usage after:  {:?}", store.get_usage(&entry, day)?);
    println!("audits: {}", store.get_recent_audits(5)?.len());
    Ok(())
}
