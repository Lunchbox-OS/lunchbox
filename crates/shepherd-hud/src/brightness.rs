//! Screen-brightness monitoring and control module
//!
//! Mirrors [`crate::volume`]: the HUD asks shepherdd for the current
//! brightness on connect, then drives changes through the daemon so policy
//! restrictions are enforced in one place.

use shepherd_api::BrightnessInfo;
use shepherd_ipc::IpcClient;
use shepherd_util::default_socket_path;
use tokio::runtime::Runtime;

/// Get current brightness status from shepherdd
pub fn get_brightness_status() -> Option<BrightnessInfo> {
    let socket_path = default_socket_path();
    let rt = match Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!("Failed to create runtime: {}", e);
            return None;
        }
    };
    rt.block_on(async {
        match IpcClient::connect(&socket_path).await {
            Ok(mut client) => match client.get_brightness().await {
                Ok(info) => Some(info),
                Err(e) => {
                    tracing::error!("Failed to get brightness: {}", e);
                    None
                }
            },
            Err(e) => {
                tracing::debug!("Failed to connect to shepherdd for brightness: {}", e);
                None
            }
        }
    })
}

/// Set brightness to a specific percentage via shepherdd
pub fn set_brightness(percent: u8) -> anyhow::Result<()> {
    let socket_path = default_socket_path();
    let rt = Runtime::new()?;
    rt.block_on(async {
        let mut client = IpcClient::connect(&socket_path).await?;
        client
            .set_brightness(percent)
            .await
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!(e.to_string()))
    })
}

/// Enable or disable automatic (ambient-light) brightness via shepherdd.
pub fn set_auto_brightness(enabled: bool) -> anyhow::Result<()> {
    let socket_path = default_socket_path();
    let rt = Runtime::new()?;
    rt.block_on(async {
        let mut client = IpcClient::connect(&socket_path).await?;
        client
            .set_auto_brightness(enabled)
            .await
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!(e.to_string()))
    })
}

#[cfg(test)]
mod tests {
    use shepherd_api::BrightnessRestrictions;

    #[test]
    fn test_brightness_icon_name() {
        // Adwaita/Yaru only ship a single `display-brightness-symbolic`,
        // so the icon name does not vary with the percentage.
        for percent in [0u8, 10, 50, 90, 100] {
            let info = shepherd_api::BrightnessInfo {
                percent,
                available: true,
                backend: Some("test".into()),
                device: Some("test0".into()),
                restrictions: BrightnessRestrictions::unrestricted(),
                auto_available: false,
                auto_enabled: false,
            };
            assert_eq!(info.icon_name(), "display-brightness-symbolic");
        }
    }
}
