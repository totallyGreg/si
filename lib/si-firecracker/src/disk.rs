use crate::errors::FirecrackerError;
use devicemapper::DM;
use devicemapper::{DevId, DmName, DmOptions};
use loopdev::{LoopControl, LoopDevice};
use nix::mount::{mount, MsFlags};
use std::fs::File;
use std::path::PathBuf;
use std::result;
use std::str::FromStr;

const OVERLAY_SZ: u64 = 5368709120;

type Result<T> = result::Result<T, FirecrackerError>;

pub struct FirecrackerDisk;

impl FirecrackerDisk {
    pub fn create_rootfs_cow(jail: &PathBuf, rootfs: &PathBuf, overlay: &str) -> Result<()> {
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

    async fn create_overlay_file(path: PathBuf) -> Result<()> {
        let overlay_file = PathBuf::from_str(&format!("{}/{}", jail.display(), overlay)).unwrap();
        let overlay_file = File::create(&overlay_file).unwrap();

        overlay_file.set_len(OVERLAY_SZ).unwrap();
    }
}
