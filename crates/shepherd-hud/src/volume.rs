//! Volume monitoring and control module
//!
//! Provides volume status and control via shepherdd. The service
//! handles actual volume control and enforces restrictions.

use shepherd_ipc::IpcClient;
use shepherd_util::default_socket_path;
use tokio::runtime::Runtime;

/// Toggle mute state via shepherdd
pub fn toggle_mute() -> anyhow::Result<()> {
    let socket_path = default_socket_path();
    let rt = Runtime::new()?;
    rt.block_on(async {
        let mut client = IpcClient::connect(&socket_path).await?;
        client
            .toggle_mute()
            .await
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!(e.to_string()))
    })
}

/// Set volume to a specific percentage via shepherdd
pub fn set_volume(percent: u8) -> anyhow::Result<()> {
    let socket_path = default_socket_path();
    let rt = Runtime::new()?;
    rt.block_on(async {
        let mut client = IpcClient::connect(&socket_path).await?;
        client
            .set_volume(percent)
            .await
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!(e.to_string()))
    })
}

#[cfg(test)]
mod tests {
    use shepherd_api::VolumeRestrictions;

    #[test]
    fn test_volume_icon_names() {
        // Test that VolumeInfo::icon_name works correctly
        let info = shepherd_api::VolumeInfo {
            percent: 0,
            muted: false,
            available: true,
            backend: Some("test".into()),
            restrictions: VolumeRestrictions::unrestricted(),
            output: None,
        };
        assert_eq!(info.icon_name(), "audio-volume-muted-symbolic");

        let info = shepherd_api::VolumeInfo {
            percent: 50,
            muted: false,
            available: true,
            backend: Some("test".into()),
            restrictions: VolumeRestrictions::unrestricted(),
            output: None,
        };
        assert_eq!(info.icon_name(), "audio-volume-medium-symbolic");

        let info = shepherd_api::VolumeInfo {
            percent: 100,
            muted: true,
            available: true,
            backend: Some("test".into()),
            restrictions: VolumeRestrictions::unrestricted(),
            output: None,
        };
        assert_eq!(info.icon_name(), "audio-volume-muted-symbolic");
    }
}
