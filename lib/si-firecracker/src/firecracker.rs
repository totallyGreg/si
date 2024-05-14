use crate::disk::FirecrackerDisk;
use crate::errors::FirecrackerError;
use crate::network::FirecrackerNetwork;

use std::{result, str::FromStr};

type Result<T> = result::Result<T, FirecrackerError>;

const JAIL_HOME: &str = "/srv/jailer/firecracker/";
const OVERLAY_SZ: u64 = 5368709120;

pub struct FirecrackerJail {
    id: u32,
    jail: PathBuf,
    loop_dev: LoopDevice,
    overlay: String,
    rootfs: PathBuf,
}

impl FirecrackerJail {
    async fn prepare(id: u32) -> Result<Self> {
        let jail: PathBuf = format!("{}/{}/root", JAIL_HOME, id).into();
        let overlay = format!("rootfs-overlay-{id}");
        let rootfs = format!("{}/rootfs.ext4", jail.display()).into();

        FirecrackerDisk::create_rootfs_cow(&jail, &rootfs, &overlay)?;
        FirecrackerNetwork::prepare(id).await?;

        Ok(Self {
            id,
            jail: jail.clone(),
            loop_dev,
            overlay,
            rootfs,
        })
    }

    pub fn stop(&self) -> Result<()> {
        nix::mount::umount(self.rootfs.into()).unwrap();
        let dm = devicemapper::DM::new().unwrap();
        // dm.device_remove(
        //     DevId {
        //         Name: self.overlay,
        //         ..Default::default()
        //     },
        //     ..Default::default(),
        // );
        self.loop_dev.detach().unwrap();
        Ok(())
    }
}
