use anyhow::Context;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::ipv4::{
    ClientConfiguration as IpClientConfiguration, Configuration as IpConfiguration,
    DHCPClientSettings,
};
use esp_idf_svc::netif::{EspNetif, NetifConfiguration, NetifStack};
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::wifi::{
    AuthMethod, BlockingWifi, ClientConfiguration, Configuration, EspWifi, WifiDriver,
};
use fennec_core::config::Config;

pub struct Wifi<'d> {
    wifi: BlockingWifi<EspWifi<'d>>,
    ssid: String,
}

impl<'d> Wifi<'d> {
    pub fn new(modem: Modem<'d>, config: &Config) -> anyhow::Result<Self> {
        let sys_loop = EspSystemEventLoop::take()?;
        let nvs = EspDefaultNvsPartition::take()?;
        let driver = WifiDriver::new(modem, sys_loop.clone(), Some(nvs))?;
        // Announce `fennec-<name>` over DHCP; many routers then resolve it,
        // which `fennec ota` can use instead of an IP address.
        let hostname = format!("fennec-{}", config.device_name);
        let netif = EspNetif::new_with_conf(&NetifConfiguration {
            ip_configuration: Some(IpConfiguration::Client(IpClientConfiguration::DHCP(
                DHCPClientSettings {
                    hostname: Some(hostname.as_str().try_into().unwrap_or_default()),
                },
            ))),
            ..NetifConfiguration::wifi_default_client()
        })?;
        let wifi = EspWifi::wrap_all(
            driver,
            netif,
            #[cfg(esp_idf_esp_wifi_softap_support)]
            EspNetif::new(NetifStack::Ap)?,
        )?;
        let mut wifi = BlockingWifi::wrap(wifi, sys_loop)?;
        let auth_method = if config.wifi_password.is_empty() {
            AuthMethod::None
        } else {
            AuthMethod::WPA2Personal
        };
        wifi.set_configuration(&Configuration::Client(ClientConfiguration {
            ssid: config.wifi_ssid.as_str().try_into().unwrap_or_default(),
            password: config.wifi_password.as_str().try_into().unwrap_or_default(),
            auth_method,
            channel: config.wifi_channel,
            ..Default::default()
        }))?;
        wifi.start()?;
        Ok(Self {
            wifi,
            ssid: config.wifi_ssid.clone(),
        })
    }

    /// Connects unless already connected.
    pub fn ensure_connected(&mut self) -> anyhow::Result<()> {
        if self.wifi.is_connected()? && self.wifi.is_up()? {
            return Ok(());
        }
        log::info!("connecting to {:?}", self.ssid);
        self.wifi
            .connect()
            .with_context(|| format!("cannot connect to Wi-Fi network {:?}", self.ssid))?;
        self.wifi.wait_netif_up()?;
        let ip = self.wifi.wifi().sta_netif().get_ip_info()?;
        log::info!("connected, address {}", ip.ip);
        Ok(())
    }
}
