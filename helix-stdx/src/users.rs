//! The names of users and groups, looked up through the C library.

use std::{
    ffi::{CStr, CString},
    mem::MaybeUninit,
    ptr,
};

/// The name of the user `uid`, if it has one.
pub fn user_name(uid: u32) -> Option<String> {
    lookup(|pwd: *mut libc::passwd, buf, len, result| {
        // SAFETY: the pointers are valid for the duration of the call, `len` is the buffer's.
        unsafe { libc::getpwuid_r(uid, pwd, buf, len, result) }
    })
    // SAFETY: a found entry's name points into the buffer, which still lives.
    .map(|(pwd, _buf)| unsafe { name(pwd.pw_name) })
}

/// The name of the group `gid`, if it has one.
pub fn group_name(gid: u32) -> Option<String> {
    lookup(|grp: *mut libc::group, buf, len, result| {
        // SAFETY: as in `user_name`.
        unsafe { libc::getgrgid_r(gid, grp, buf, len, result) }
    })
    // SAFETY: as in `user_name`.
    .map(|(grp, _buf)| unsafe { name(grp.gr_name) })
}

/// The id of the user called `name`.
pub fn user_id(name: &str) -> Option<u32> {
    let name = CString::new(name).ok()?;
    lookup(|pwd: *mut libc::passwd, buf, len, result| {
        // SAFETY: as in `user_name`; `name` is a valid C string.
        unsafe { libc::getpwnam_r(name.as_ptr(), pwd, buf, len, result) }
    })
    .map(|(pwd, _buf)| pwd.pw_uid)
}

/// The id of the group called `name`.
pub fn group_id(name: &str) -> Option<u32> {
    let name = CString::new(name).ok()?;
    lookup(|grp: *mut libc::group, buf, len, result| {
        // SAFETY: as in `user_id`.
        unsafe { libc::getgrnam_r(name.as_ptr(), grp, buf, len, result) }
    })
    .map(|(grp, _buf)| grp.gr_gid)
}

/// The user the editor runs as.
pub fn current_user() -> u32 {
    rustix::process::getuid().as_raw()
}

/// The groups the editor runs in: its group and its supplementary groups.
pub fn current_groups() -> Vec<u32> {
    let mut groups: Vec<u32> = rustix::process::getgroups()
        .unwrap_or_default()
        .into_iter()
        .map(|gid| gid.as_raw())
        .collect();
    groups.push(rustix::process::getgid().as_raw());
    groups.sort_unstable();
    groups.dedup();
    groups
}

/// Runs one of the reentrant `get*_r` lookups. Returns the entry and the buffer its strings point
/// into, or `None` if there is no such entry.
fn lookup<T>(
    mut call: impl FnMut(*mut T, *mut libc::c_char, libc::size_t, *mut *mut T) -> libc::c_int,
) -> Option<(T, Vec<libc::c_char>)> {
    let mut buf = vec![0; 1024];
    loop {
        let mut entry = MaybeUninit::<T>::uninit();
        let mut result = ptr::null_mut();
        let code = call(entry.as_mut_ptr(), buf.as_mut_ptr(), buf.len(), &mut result);
        if code == libc::ERANGE && buf.len() < 1 << 20 {
            buf.resize(buf.len() * 2, 0);
            continue;
        }
        if code != 0 || result.is_null() {
            return None;
        }
        // SAFETY: the lookup succeeded, so it initialized `entry`.
        return Some((unsafe { entry.assume_init() }, buf));
    }
}

/// # Safety
///
/// `name` must point to a valid C string.
unsafe fn name(name: *const libc::c_char) -> String {
    // SAFETY: the caller guarantees a valid C string.
    unsafe { CStr::from_ptr(name) }
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_is_found_both_ways() {
        assert_eq!(user_name(0).as_deref(), Some("root"));
        assert_eq!(user_id("root"), Some(0));
        let group = group_name(0).unwrap();
        assert_eq!(group_id(&group), Some(0));
        assert_eq!(user_id("no such user, surely"), None);
        assert!(current_groups().contains(&rustix::process::getgid().as_raw()));
    }
}
