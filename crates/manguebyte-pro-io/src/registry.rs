//! The installed drivers, as the registry lists them.

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use crate::Error;
use crate::sys::Guid;

/// An installed driver, as listed under `HKEY_LOCAL_MACHINE\SOFTWARE\ASIO`.
///
/// ASIO is a trademark of Steinberg Media Technologies GmbH, registered in Europe and other countries.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DriverEntry {
    /// The name of the driver's registry key, which identifies it.
    pub name: String,
    /// The key's `Description` value, a name to show; the key's name when
    /// it has none.
    pub description: String,
    /// The COM class the driver is created from.
    pub clsid: Clsid,
    /// The driver's library, as COM registered it.
    pub library: Option<PathBuf>,
}

/// A COM class identifier, written as `{xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Clsid(pub(crate) Guid);

impl fmt::Debug for Clsid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Clsid({self})")
    }
}

impl Clsid {
    /// The identifier from its sixteen bytes, in the order COM keeps them.
    pub fn from_fields(data1: u32, data2: u16, data3: u16, data4: [u8; 8]) -> Self {
        Self(Guid {
            data1,
            data2,
            data3,
            data4,
        })
    }
}

/// Text that is not a class identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseClsidError;

impl fmt::Display for ParseClsidError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("not a class identifier of the form {xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}")
    }
}

impl std::error::Error for ParseClsidError {}

impl FromStr for Clsid {
    type Err = ParseClsidError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let text = text.trim();
        let inner = text
            .strip_prefix('{')
            .and_then(|rest| rest.strip_suffix('}'))
            .unwrap_or(text);
        let groups: Vec<&str> = inner.split('-').collect();
        let lengths = [8, 4, 4, 4, 12];
        if groups.len() != 5
            || groups
                .iter()
                .zip(lengths)
                .any(|(group, length)| group.len() != length)
            || !inner.chars().all(|c| c == '-' || c.is_ascii_hexdigit())
        {
            return Err(ParseClsidError);
        }
        let hex = |text: &str| u64::from_str_radix(text, 16).map_err(|_| ParseClsidError);
        let tail = (hex(groups[3])? << 48) | hex(groups[4])?;
        Ok(Self(Guid {
            data1: hex(groups[0])? as u32,
            data2: hex(groups[1])? as u16,
            data3: hex(groups[2])? as u16,
            data4: tail.to_be_bytes(),
        }))
    }
}

impl fmt::Display for Clsid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Guid {
            data1,
            data2,
            data3,
            data4,
        } = self.0;
        write!(f, "{{{data1:08X}-{data2:04X}-{data3:04X}-")?;
        for byte in &data4[..2] {
            write!(f, "{byte:02X}")?;
        }
        f.write_str("-")?;
        for byte in &data4[2..] {
            write!(f, "{byte:02X}")?;
        }
        f.write_str("}")
    }
}

/// Lists the installed drivers, in the registry's order.
///
/// A driver whose library COM does not know, or whose file is missing, is
/// left out, as Steinberg's own host code leaves it out. Elsewhere than on
/// Windows the list is empty.
pub fn drivers() -> Result<Vec<DriverEntry>, Error> {
    #[cfg(windows)]
    {
        crate::com::list_drivers()
    }
    #[cfg(not(windows))]
    {
        Ok(Vec::new())
    }
}

/// The installed driver with this name: its key's name or its description,
/// in any case.
pub fn find_driver(name: &str) -> Result<DriverEntry, Error> {
    let wanted = name.trim();
    drivers()?
        .into_iter()
        .find(|entry| {
            entry.name.eq_ignore_ascii_case(wanted)
                || entry.description.eq_ignore_ascii_case(wanted)
        })
        .ok_or_else(|| Error::DriverNotFound(name.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_class_identifier_round_trips_through_text() {
        let text = "{0351302F-B1F1-4A5D-8613-787F77C20EA4}";
        let clsid: Clsid = text.parse().unwrap();
        assert_eq!(clsid.0.data1, 0x0351_302f);
        assert_eq!(clsid.0.data2, 0xb1f1);
        assert_eq!(clsid.0.data3, 0x4a5d);
        assert_eq!(
            clsid.0.data4,
            [0x86, 0x13, 0x78, 0x7f, 0x77, 0xc2, 0x0e, 0xa4]
        );
        assert_eq!(clsid.to_string(), text);
        assert_eq!(
            "232685c6-6548-49d8-846d-4141a3ef7560".parse::<Clsid>(),
            Ok(Clsid::from_fields(
                0x2326_85c6,
                0x6548,
                0x49d8,
                [0x84, 0x6d, 0x41, 0x41, 0xa3, 0xef, 0x75, 0x60]
            ))
        );
        for bad in [
            "",
            "{}",
            "{0351302F-B1F1-4A5D-8613-787F77C20EA}",
            "{0351302F-B1F1-4A5D-8613+787F77C20EA4}",
            "{0351302G-B1F1-4A5D-8613-787F77C20EA4}",
            "{+351302F-B1F1-4A5D-8613-787F77C20EA4}",
        ] {
            assert_eq!(bad.parse::<Clsid>(), Err(ParseClsidError), "{bad}");
        }
    }
}
