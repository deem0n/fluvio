#[cfg(any(feature = "cluster_components", feature = "cluster_components_rustls"))]
mod local;
mod k8;
mod tls;

use fmt::Display;
use structopt::StructOpt;
use std::{fmt, str::FromStr};

use crate::Terminal;
use crate::CliError;
use tls::TlsOpt;

#[cfg(target_os = "macos")]
fn get_log_directory() -> &'static str {
    "/usr/local/var/log/fluvio"
}

#[cfg(not(target_os = "macos"))]
fn get_log_directory() -> &'static str {
    "/tmp"
}

#[derive(Debug)]
pub struct DefaultVersion(String);

impl Default for DefaultVersion {
    fn default() -> Self {
        Self(crate::VERSION.to_string())
    }
}

impl Display for DefaultVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for DefaultVersion {
    type Err = std::io::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(s.to_string()))
    }
}

#[derive(Debug)]
pub struct DefaultLogDirectory(String);

impl Default for DefaultLogDirectory {
    fn default() -> Self {
        Self(get_log_directory().to_string())
    }
}

impl Display for DefaultLogDirectory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for DefaultLogDirectory {
    type Err = std::io::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(s.to_string()))
    }
}

#[derive(Debug, StructOpt)]
pub struct K8Install {
    /// k8: use specific chart version
    #[structopt(long, default_value)]
    pub chart_version: DefaultVersion,

    /// k8: use specific image version
    #[structopt(long)]
    pub image_version: Option<String>,

    /// k8: use custom docker registry
    #[structopt(long)]
    pub registry: Option<String>,

    /// k8
    #[structopt(long, default_value = "default")]
    pub namespace: String,

    /// k8
    #[structopt(long, default_value = "main")]
    pub group_name: String,

    /// helm chart installation name
    #[structopt(long, default_value = "fluvio")]
    pub install_name: String,

    /// Local path to a helm chart to install
    #[structopt(long)]
    pub chart_location: Option<String>,

    /// k8
    #[structopt(long, default_value = "minikube")]
    pub cloud: String,
}

#[derive(Debug, StructOpt)]
pub struct InstallCommand {
    /// use local image
    #[structopt(long)]
    pub develop: bool,

    #[structopt(flatten)]
    pub k8_config: K8Install,

    #[structopt(long)]
    pub skip_profile_creation: bool,

    /// number of SPU
    #[structopt(long, default_value = "1")]
    pub spu: u16,

    /// RUST_LOG options
    #[structopt(long)]
    pub rust_log: Option<String>,

    /// log dir
    #[structopt(long, default_value)]
    log_dir: DefaultLogDirectory,

    #[structopt(long)]
    /// installing sys
    sys: bool,

    /// install local spu/sc(custom)
    #[structopt(long)]
    local: bool,

    #[structopt(flatten)]
    tls: TlsOpt,

    #[structopt(long)]
    authorization_config_map: Option<String>,
}

pub async fn process_install<O>(
    _out: std::sync::Arc<O>,
    command: InstallCommand,
) -> Result<String, CliError>
where
    O: Terminal,
{
    use k8::install_sys;
    use k8::install_core;

    let spu = command.spu;

    #[cfg(any(feature = "cluster_components", feature = "cluster_components_rustls"))]
    use local::install_local;

    if command.sys {
        install_sys(command)?;
    } else if command.local {
        #[cfg(any(feature = "cluster_components", feature = "cluster_components_rustls"))]
        install_local(command).await?;
        confirm_spu(spu).await?;
    } else {
        install_core(command).await?;
        confirm_spu(spu).await?;
    }

    Ok("".to_owned())
}

/// check to ensure spu are all running
async fn confirm_spu(spu: u16) -> Result<(), CliError> {
    use std::time::Duration;

    use fluvio_future::timer::sleep;
    use fluvio::Fluvio;
    use fluvio_cluster::ClusterError;
    use fluvio_controlplane_metadata::spu::SpuSpec;

    // sleep 1 second to allow spu to spin up just in case
    sleep(Duration::from_secs(1)).await;

    let client = Fluvio::connect().await.expect("sc ");
    let mut admin = client.admin().await;

    // wait for list of spu
    for _ in 0..30u16 {
        let spus = admin.list::<SpuSpec, _>(vec![]).await.expect("no spu list");
        let live_spus = spus
            .iter()
            .filter(|spu| spu.status.is_online() && !spu.spec.public_endpoint.ingress.is_empty())
            .count();
        if live_spus == spu as usize {
            println!("{} spus provisioned", spus.len());
            drop(client);
            sleep(Duration::from_millis(1)).await; // give destructor time to clean up properly
            return Ok(());
        } else {
            println!("{} out of spu: {} up, waiting 5 sec", live_spus, spu);
            sleep(Duration::from_secs(5)).await;
        }
    }

    //drop(admin);

    println!("waited too long,bailing out");
    Err(ClusterError::Other(format!("not able to provision:{} spu", spu)).into())
}
