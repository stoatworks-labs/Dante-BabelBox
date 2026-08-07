//! Runtime discovery of a device's object tree.
//!
//! AES70 leaves ONo allocation entirely to the vendor, so the only
//! vendor-neutral way to find "the gain control for input 3" is to ask the
//! device: walk `OcaBlock::GetMembers` down from the root block, and read each
//! object's class and role back off the wire. That is what this module does,
//! and it is why this crate carries no per-vendor ONo map.
//!
//! **Defensive by design.** Two things here are not verified against hardware:
//! the method index of `GetMembers` (see [`crate::classes::block`]) and the
//! exact marshalling of its reply. Both are handled by trying the most likely
//! form and falling back rather than failing the enumeration:
//!
//! - the method index is tried against
//!   [`crate::classes::block::GET_MEMBERS_CANDIDATES`] in order, skipping past
//!   `BadMethod`/`NotImplemented`;
//! - the reply is parsed first as `OcaList<OcaObjectIdentification>` (ONo plus
//!   inline class) and, if that doesn't consume the buffer cleanly, as a plain
//!   `OcaList<OcaONo>` with each object's class fetched individually.
//!
//! A device that answers neither yields an empty enumeration, not an error the
//! caller can do nothing about.

use std::collections::HashSet;

use tracing::debug;

use crate::classes::{block, ClassIdentification};
use crate::client::Client;
use crate::ono::{reserved, Ono};
use crate::value::Reader;
use crate::Error;

/// How deep to recurse into nested blocks before giving up. A real device's
/// tree is two or three levels; anything deeper is a malformed reply or a loop.
const MAX_DEPTH: usize = 8;
/// Ceiling on total objects, so a garbled length prefix can't spin forever.
const MAX_OBJECTS: usize = 4096;

/// One object found on a device, with everything needed to classify it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredObject {
    pub ono: Ono,
    pub class: ClassIdentification,
    /// The object's own `OcaRoot::Role`, or its ONo in hex if the device
    /// wouldn't give one up.
    pub role: String,
    /// Roles of the blocks containing this object, outermost first. A device
    /// that names its blocks per input ("Input 3") puts the channel number
    /// here rather than in the leaf role.
    pub path: Vec<String>,
}

impl DiscoveredObject {
    /// Role and container path joined, for logging and for role matching that
    /// wants to see the whole context in one string.
    pub fn qualified_role(&self) -> String {
        if self.path.is_empty() {
            self.role.clone()
        } else {
            format!("{}/{}", self.path.join("/"), self.role)
        }
    }
}

/// A member as it comes back from `GetMembers`, before its role is fetched.
#[derive(Debug, Clone)]
struct Member {
    ono: Ono,
    class: Option<ClassIdentification>,
}

/// Walk the whole object tree from the root block.
pub async fn enumerate(client: &Client) -> Result<Vec<DiscoveredObject>, Error> {
    enumerate_from(client, reserved::ROOT_BLOCK).await
}

/// Walk the object tree from an arbitrary block.
pub async fn enumerate_from(client: &Client, root: Ono) -> Result<Vec<DiscoveredObject>, Error> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    seen.insert(root);
    walk(client, root, &mut Vec::new(), 0, &mut seen, &mut out).await?;
    Ok(out)
}

/// Recursive step. Written as an explicit boxed future because `async fn`
/// cannot recurse directly.
fn walk<'a>(
    client: &'a Client,
    container: Ono,
    path: &'a mut Vec<String>,
    depth: usize,
    seen: &'a mut HashSet<Ono>,
    out: &'a mut Vec<DiscoveredObject>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), Error>> + Send + 'a>> {
    Box::pin(async move {
        if depth >= MAX_DEPTH || out.len() >= MAX_OBJECTS {
            return Ok(());
        }

        let members = match get_members(client, container).await {
            Ok(members) => members,
            // A block that won't enumerate is a dead end, not a failed device:
            // its siblings may still answer.
            Err(e) => {
                debug!(container = %container, error = %e, "block did not enumerate");
                return Ok(());
            }
        };

        for member in members {
            if out.len() >= MAX_OBJECTS {
                return Ok(());
            }
            if !seen.insert(member.ono) {
                continue;
            }

            let class = match member.class {
                Some(class) => class,
                None => match client.class_of(member.ono).await {
                    Ok(class) => class,
                    Err(e) => {
                        debug!(ono = %member.ono, error = %e, "object has no class identification");
                        continue;
                    }
                },
            };

            let role = client
                .role_of(member.ono)
                .await
                .unwrap_or_else(|_| format!("{}", member.ono));

            if class.is_block() {
                path.push(role.clone());
                walk(client, member.ono, path, depth + 1, seen, out).await?;
                path.pop();
            } else {
                out.push(DiscoveredObject {
                    ono: member.ono,
                    class,
                    role,
                    path: path.clone(),
                });
            }
        }

        Ok(())
    })
}

/// `OcaBlock::GetMembers`, tolerant of both the method index and the reply
/// shape — see the module comment.
async fn get_members(client: &Client, container: Ono) -> Result<Vec<Member>, Error> {
    let mut last_err = None;

    for method in block::GET_MEMBERS_CANDIDATES {
        match client.request(container, *method, 0, Vec::new()).await {
            Ok(response) => return Ok(parse_members(&response.params)),
            // Only a "you asked for the wrong method" answer justifies trying
            // the next candidate; anything else is a real failure.
            Err(Error::Status { code: 8 | 11, .. }) => {
                last_err = Some(Error::Status { code: 11, name: "BadMethod" });
                continue;
            }
            Err(e) => return Err(e),
        }
    }

    Err(last_err.unwrap_or(Error::Status { code: 11, name: "BadMethod" }))
}

/// Parse a `GetMembers` reply, preferring the richer form.
fn parse_members(params: &[u8]) -> Vec<Member> {
    if let Some(members) = parse_object_identifications(params) {
        return members;
    }
    parse_bare_onos(params).unwrap_or_default()
}

/// `OcaList<OcaObjectIdentification>`, i.e. ONo followed by an inline
/// `OcaClassIdentification` (`OcaClassID` as a u16-counted list of u16 fields,
/// then a u32 version).
fn parse_object_identifications(params: &[u8]) -> Option<Vec<Member>> {
    let mut r = Reader::new(params);
    let members = r
        .list(|r| {
            let ono = Ono(r.u32()?);
            let fields = r.list(|r| r.u16())?;
            let version = r.u32()?;
            // A zero-field class id means we've drifted out of alignment and
            // are reading padding, not a class.
            if fields.is_empty() {
                return Err(Error::Truncated);
            }
            Ok(Member { ono, class: Some(ClassIdentification { fields, version }) })
        })
        .ok()?;

    // Only trust this reading if it accounted for the whole buffer.
    r.is_empty().then_some(members)
}

/// `OcaList<OcaONo>` — the degenerate form, where the class has to be fetched
/// per object afterwards.
fn parse_bare_onos(params: &[u8]) -> Option<Vec<Member>> {
    let mut r = Reader::new(params);
    let members = r.list(|r| Ok(Member { ono: Ono(r.u32()?), class: None })).ok()?;
    r.is_empty().then_some(members)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Writer;

    fn object_identification(w: &mut Writer, ono: u32, fields: &[u16], version: u32) {
        w.u32(ono).u16(fields.len() as u16);
        for f in fields {
            w.u16(*f);
        }
        w.u32(version);
    }

    #[test]
    fn parses_the_rich_member_form() {
        let mut w = Writer::new();
        w.u16(2);
        object_identification(&mut w, 0x1001, &[1, 1, 1, 5], 2); // OcaGain
        object_identification(&mut w, 0x1002, &[1, 1, 3], 2); // OcaBlock
        let members = parse_members(&w.finish());

        assert_eq!(members.len(), 2);
        assert_eq!(members[0].ono, Ono(0x1001));
        assert_eq!(members[0].class.as_ref().unwrap().name(), Some("OcaGain"));
        assert!(members[1].class.as_ref().unwrap().is_block());
    }

    #[test]
    fn falls_back_to_bare_onos_when_the_rich_parse_does_not_fit() {
        let mut w = Writer::new();
        w.u16(3).u32(10).u32(20).u32(30);
        let members = parse_members(&w.finish());

        assert_eq!(members.len(), 3);
        assert_eq!(members.iter().map(|m| m.ono.0).collect::<Vec<_>>(), vec![10, 20, 30]);
        // No inline class: the caller has to ask the device for each one.
        assert!(members.iter().all(|m| m.class.is_none()));
    }

    /// A rich-form buffer is also a valid *prefix* of nothing else, so the
    /// "did it consume everything" check is what keeps the two apart. Trailing
    /// bytes must reject the rich reading rather than silently truncating it.
    #[test]
    fn a_partially_consumed_buffer_is_not_accepted_as_the_rich_form() {
        let mut w = Writer::new();
        w.u16(1);
        object_identification(&mut w, 0x1001, &[1, 1, 1, 5], 2);
        w.raw(&[0xFF, 0xFF]); // trailing junk
        assert!(parse_object_identifications(&w.finish()).is_none());
    }

    #[test]
    fn an_empty_list_parses_to_no_members() {
        let mut w = Writer::new();
        w.u16(0);
        assert!(parse_members(&w.finish()).is_empty());
    }

    #[test]
    fn garbage_yields_no_members_rather_than_an_error() {
        assert!(parse_members(&[0xFF, 0xFF, 0x01]).is_empty());
    }

    #[test]
    fn qualified_role_joins_the_container_path() {
        let object = DiscoveredObject {
            ono: Ono(0x1001),
            class: ClassIdentification { fields: vec![1, 1, 1, 5], version: 2 },
            role: "Gain".into(),
            path: vec!["Inputs".into(), "Input 3".into()],
        };
        assert_eq!(object.qualified_role(), "Inputs/Input 3/Gain");
    }
}
