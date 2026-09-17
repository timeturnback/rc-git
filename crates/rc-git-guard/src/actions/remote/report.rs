//! The send-pack command line and the `report-status` reply, as pure functions over
//! bytes. gix has no push protocol, so this is written against git's documented
//! `pack-protocol` format and tested with hand-built replies.

use gix::ObjectId;

/// The first (and only) command pkt-line of a plain single-ref push:
/// `<old> <new> <full-ref>\0<capabilities>`. `old` is the zero id when the remote
/// does not have the ref yet.
pub(super) fn command_line(old: Option<ObjectId>, new: ObjectId, full_ref: &str) -> Vec<u8> {
    let zero = ObjectId::null(new.kind());
    let old = old.unwrap_or(zero);
    let mut line = format!("{old} {new} {full_ref}").into_bytes();
    line.push(0);
    line.extend_from_slice(b"report-status agent=rc-git/");
    line.extend_from_slice(env!("CARGO_PKG_VERSION").as_bytes());
    line
}

/// How the server answered a push.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Report {
    /// The pack was unpacked and the ref updated.
    Ok,
    /// The server refused: `unpack` failed or the ref update was rejected with the
    /// given reason (e.g. `non-fast-forward`).
    Refused {
        /// The server's reason, verbatim.
        reason: String,
        /// Whether the reason names a non-fast-forward rejection.
        non_fast_forward: bool,
    },
}

/// Parses a `report-status` reply. `payload` is the concatenated pkt-line payloads
/// (framing already stripped by the transport); `full_ref` is the one ref pushed.
pub(super) fn parse_report(payload: &[u8], full_ref: &str) -> Result<Report, String> {
    let text = String::from_utf8_lossy(payload);
    let mut unpack_ok = false;
    let mut ref_status: Option<Report> = None;
    for line in text.lines() {
        let line = line.trim_end();
        if let Some(status) = line.strip_prefix("unpack ") {
            if status == "ok" {
                unpack_ok = true;
            } else {
                return Ok(Report::Refused {
                    reason: format!("unpack {status}"),
                    non_fast_forward: false,
                });
            }
        } else if let Some(rest) = line.strip_prefix("ok ") {
            if rest == full_ref {
                ref_status = Some(Report::Ok);
            }
        } else if let Some(rest) = line.strip_prefix("ng ") {
            let (name, reason) = rest.split_once(' ').unwrap_or((rest, "rejected"));
            if name == full_ref {
                ref_status = Some(Report::Refused {
                    reason: reason.to_owned(),
                    non_fast_forward: reason.contains("non-fast-forward"),
                });
            }
        }
    }
    if !unpack_ok {
        return Err("the server sent no 'unpack' status".to_owned());
    }
    ref_status.ok_or_else(|| format!("the server sent no status for '{full_ref}'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oid(byte: u8) -> ObjectId {
        ObjectId::try_from(&[byte; 20][..]).expect("20 bytes")
    }

    #[test]
    fn command_line_uses_the_zero_id_for_a_new_ref() {
        let line = command_line(None, oid(0xab), "refs/heads/main");
        let text = String::from_utf8_lossy(&line);
        assert!(text.starts_with(&format!(
            "{} {} refs/heads/main\0",
            "0".repeat(40),
            "ab".repeat(20)
        )));
        assert!(text.contains("report-status agent=rc-git/"));
    }

    #[test]
    fn command_line_carries_the_old_id() {
        let line = command_line(Some(oid(0x01)), oid(0x02), "refs/heads/topic");
        assert!(String::from_utf8_lossy(&line).starts_with(&format!(
            "{} {} refs/heads/topic\0",
            "01".repeat(20),
            "02".repeat(20)
        )));
    }

    #[test]
    fn a_successful_report_parses() {
        let report = parse_report(b"unpack ok\nok refs/heads/main\n", "refs/heads/main");
        assert_eq!(report, Ok(Report::Ok));
    }

    #[test]
    fn a_rejection_carries_the_reason() {
        let report = parse_report(
            b"unpack ok\nng refs/heads/main non-fast-forward\n",
            "refs/heads/main",
        );
        assert_eq!(
            report,
            Ok(Report::Refused {
                reason: "non-fast-forward".to_owned(),
                non_fast_forward: true,
            })
        );
    }

    #[test]
    fn an_unpack_failure_is_refused() {
        let report = parse_report(b"unpack index-pack failed\n", "refs/heads/main");
        assert_eq!(
            report,
            Ok(Report::Refused {
                reason: "unpack index-pack failed".to_owned(),
                non_fast_forward: false,
            })
        );
    }

    #[test]
    fn a_report_without_our_ref_is_an_error() {
        assert!(parse_report(b"unpack ok\nok refs/heads/other\n", "refs/heads/main").is_err());
        assert!(parse_report(b"", "refs/heads/main").is_err());
    }

    #[test]
    fn a_hook_rejection_without_reason_still_parses() {
        let report = parse_report(b"unpack ok\nng refs/heads/main\n", "refs/heads/main");
        assert_eq!(
            report,
            Ok(Report::Refused {
                reason: "rejected".to_owned(),
                non_fast_forward: false,
            })
        );
    }
}
