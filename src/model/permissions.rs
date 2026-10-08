//! UNIX permission bits as shown in the Properties dialog: read / write /
//! execute for the owner, the group and everyone else, plus "Allow
//! executing as program".
//!
//! Only the permission bits (`0o7777`) are ever changed; the file type
//! bits that `unix::mode` also carries are kept out of the result.

/// The permission bits of a `unix::mode` (setuid, setgid, sticky, rwx×3).
pub const PERMISSION_BITS: u32 = 0o7777;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Owner,
    Group,
    Others,
}

impl Class {
    pub const ALL: [Self; 3] = [Self::Owner, Self::Group, Self::Others];

    fn shift(self) -> u32 {
        match self {
            Self::Owner => 6,
            Self::Group => 3,
            Self::Others => 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
    Execute,
}

impl Access {
    pub const ALL: [Self; 3] = [Self::Read, Self::Write, Self::Execute];

    fn bit(self) -> u32 {
        match self {
            Self::Read => 0o4,
            Self::Write => 0o2,
            Self::Execute => 0o1,
        }
    }
}

fn bit(class: Class, access: Access) -> u32 {
    access.bit() << class.shift()
}

pub fn has(mode: u32, class: Class, access: Access) -> bool {
    mode & bit(class, access) != 0
}

/// `mode` with one permission turned on or off.
pub fn with(mode: u32, class: Class, access: Access, on: bool) -> u32 {
    let mode = mode & PERMISSION_BITS;
    if on {
        mode | bit(class, access)
    } else {
        mode & !bit(class, access)
    }
}

/// Whether anyone may execute the file.
pub fn is_executable(mode: u32) -> bool {
    mode & 0o111 != 0
}

/// "Allow executing as program": on, everyone who can read the file may
/// execute it (like `chmod +x` with the usual umask); off, nobody may.
pub fn set_executable(mode: u32, on: bool) -> u32 {
    let mode = mode & PERMISSION_BITS;
    if on {
        // The read bits shifted onto the execute bits, the owner always.
        mode | ((mode & 0o444) >> 2) | 0o100
    } else {
        mode & !0o111
    }
}

/// `ls`-style text, e.g. "rwxr-xr-x", with the setuid/setgid/sticky
/// letters (`s`, `S`, `t`, `T`) where they apply.
pub fn symbolic(mode: u32) -> String {
    let special = [(0o4000, 's'), (0o2000, 's'), (0o1000, 't')];
    let mut text = String::with_capacity(9);
    for (i, class) in Class::ALL.into_iter().enumerate() {
        for access in Access::ALL {
            let set = has(mode, class, access);
            let letter = match access {
                Access::Read => 'r',
                Access::Write => 'w',
                Access::Execute => {
                    let (flag, letter) = special[i];
                    if mode & flag != 0 {
                        text.push(if set {
                            letter
                        } else {
                            letter.to_ascii_uppercase()
                        });
                        continue;
                    }
                    'x'
                }
            };
            text.push(if set { letter } else { '-' });
        }
    }
    text
}

/// "755", "1777": the octal form used by `chmod`.
pub fn octal(mode: u32) -> String {
    let mode = mode & PERMISSION_BITS;
    if mode > 0o777 {
        format!("{mode:04o}")
    } else {
        format!("{mode:03o}")
    }
}
