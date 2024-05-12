use crate::errors::FirecrackerError;
use devicemapper::{DevId, DmName, DmOptions};
use devicemapper::{DmTable, DmTarget, DmTransaction, DmUdev, Result, DM};

use loopdev::{LoopControl, LoopDevice};
use nix::mount::{mount, umount, MsFlags};
use std::ffi::CString;
use std::fs::File;
use std::path::PathBuf;
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
    pub fn prepare(id: u32) -> Result<Self> {
        let jail: PathBuf = format!("{}/{}/root", JAIL_HOME, id).into();
        let overlay = format!("rootfs-overlay-{id}");
        let rootfs = format!("{}/rootfs.ext4", jail.display()).into();

        FirecrackerJail::create_rootfs_cow(&jail, &rootfs, &overlay)?;

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

    fn create_rootfs_cow(jail: &PathBuf, rootfs: &PathBuf, overlay: &str) -> Result<()> {
        let overlay_file = PathBuf::from_str(&format!("{}/{}", jail.display(), overlay)).unwrap();
        let overlay_file = File::create(&overlay_file).unwrap();

        overlay_file.set_len(OVERLAY_SZ).unwrap();
        let next_loop = LoopControl::open().unwrap().next_free().unwrap();
        let loop_dev = LoopDevice::open(next_loop.path().unwrap()).unwrap();

        let dm = DM::new().unwrap();
        let dm_name = DmName::new(&overlay);
        let dev_id = DevId::Name(dm_name);
        dm.device_create(name, None, DmOptions::default()).unwrap();
        dm.table_load(
            &dev_id,
            vec![
                0,
                OVERLAY_SZ,
                "snapshot".into(),
                "/dev/mapper/rootfs".into(),
                loop_dev.path().into(),
                "P".into(),
                8,
            ],
            DmOptions::default(),
        )
        .unwrap();

        mount(
            Some(format!("/dev/mapper/{}", overlay).into()),
            &rootfs,
            None,
            MsFlags::MS_BIND,
            None,
        );

        Ok(())
    }
}
