use std::path::Path;

use sysinfo::Disks;

pub fn get_available_space(path: &Path) -> Option<u64> {
    let disks = Disks::new_with_refreshed_list();

    disks
        .iter()
        .find(|disk| path.starts_with(disk.mount_point()))
        .map(|disk| disk.available_space())
}
