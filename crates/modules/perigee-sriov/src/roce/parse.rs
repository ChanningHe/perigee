//! Parsers for the PF state reported by `dcb`, `ethtool` and rdma_cm configfs.

use super::{Pause, NUM_PRIORITIES};

/// DSCP -> priority pairs from `dcb -j app show dev <pf>`.
///
/// iproute2 prints `{}` for an empty table. Since at least 6.15 each entry is
/// `[pid, prio, " "]`: `dcb_app_print_filtered` emits a separator string into
/// every inner JSON array. Trailing whitespace-only strings are tolerated;
/// any other shape is rejected.
pub fn parse_dcb_app_json(json: &str) -> Result<Vec<(u8, u8)>, String> {
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("invalid `dcb app` JSON: {e}"))?;
    let obj = v
        .as_object()
        .ok_or_else(|| format!("`dcb app` JSON is not an object: {v}"))?;
    let Some(entries) = obj.get("dscp_prio") else {
        return Ok(Vec::new());
    };
    entries
        .as_array()
        .ok_or("`dcb app` dscp_prio is not an array")?
        .iter()
        .map(parse_dscp_prio_entry)
        .collect()
}

fn parse_dscp_prio_entry(entry: &serde_json::Value) -> Result<(u8, u8), String> {
    let bad = || format!("unexpected `dcb app` dscp_prio entry {entry}");
    let items = entry.as_array().ok_or_else(bad)?;
    let [pid, prio, rest @ ..] = items.as_slice() else {
        return Err(bad());
    };
    let is_separator = |x: &serde_json::Value| x.as_str().is_some_and(|s| s.trim().is_empty());
    if !rest.iter().all(is_separator) {
        return Err(bad());
    }
    let num = |x: &serde_json::Value| {
        x.as_u64()
            .and_then(|n| u8::try_from(n).ok())
            .ok_or_else(bad)
    };
    Ok((num(pid)?, num(prio)?))
}

/// Per-priority PFC enable vector from `dcb -j pfc show dev <pf>`.
pub fn parse_dcb_pfc_json(json: &str) -> Result<[bool; NUM_PRIORITIES], String> {
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("invalid `dcb pfc` JSON: {e}"))?;
    let bools: Vec<bool> = v
        .get("prio_pfc")
        .and_then(|p| p.as_array())
        .ok_or("`dcb pfc` output has no prio_pfc array")?
        .iter()
        .map(|b| b.as_bool().ok_or("`dcb pfc` prio_pfc holds a non-bool"))
        .collect::<Result<_, _>>()?;
    bools
        .try_into()
        .map_err(|v: Vec<bool>| format!("`dcb pfc` prio_pfc has {} entries, want 8", v.len()))
}

/// RX/TX pause from `ethtool -a <pf>` ("RX:\t\ton"). The "RX negotiated:"
/// lines printed with autoneg are ignored.
pub fn parse_pause(out: &str) -> Result<Pause, String> {
    let field = |name: &str| -> Result<bool, String> {
        let value = labelled_value(out, name)
            .ok_or_else(|| format!("`ethtool -a` output has no {name} line"))?;
        match value {
            "on" => Ok(true),
            "off" => Ok(false),
            other => Err(format!(
                "`ethtool -a` {name} has unexpected value '{other}'"
            )),
        }
    };
    Ok(Pause {
        rx: field("RX")?,
        tx: field("TX")?,
    })
}

/// Numeric value of `<name>: <n>` from `ethtool --get-tunable`.
pub fn parse_tunable(out: &str, name: &str) -> Result<u16, String> {
    let value = labelled_value(out, name)
        .ok_or_else(|| format!("`ethtool --get-tunable` output has no {name}"))?;
    value
        .parse()
        .map_err(|_| format!("{name} has non-numeric value '{value}'"))
}

/// Content of an rdma_cm `default_roce_tos` configfs file.
pub fn parse_tos(content: &str) -> Result<u8, String> {
    content
        .trim()
        .parse()
        .map_err(|_| format!("unexpected default_roce_tos value '{}'", content.trim()))
}

/// Value of the first `<label>: <value>` line whose label matches exactly.
fn labelled_value<'a>(out: &'a str, label: &str) -> Option<&'a str> {
    out.lines()
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.trim() == label)
        .map(|(_, v)| v.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Fixtures captured on serverhub (iproute2-6.15.0, ethtool 6.14.2) unless
    // noted otherwise.

    #[test]
    fn dcb_app_json_empty_object_means_no_entries() {
        assert_eq!(parse_dcb_app_json("{}\n"), Ok(vec![]));
    }

    #[test]
    fn dcb_app_json_reads_iproute2_6_15_shape() {
        // Shape from iproute2-6.15.0 dcb/dcb_app.c (dcb_app_print_filtered
        // prints a " " separator into each entry). Not captured from hardware:
        // the host has no DSCP entries.
        let json = r#"{"dscp_prio":[[26,3," "],[48,6," "]]}"#;
        assert_eq!(parse_dcb_app_json(json), Ok(vec![(26, 3), (48, 6)]));
    }

    #[test]
    fn dcb_app_json_reads_plain_pairs() {
        let json = r#"{"dscp_prio":[[26,3],[48,6]]}"#;
        assert_eq!(parse_dcb_app_json(json), Ok(vec![(26, 3), (48, 6)]));
    }

    #[test]
    fn dcb_app_json_ignores_other_selectors() {
        let json = r#"{"ethtype_prio":[[35078,3," "]],"dscp_prio":[[10,1," "]]}"#;
        assert_eq!(parse_dcb_app_json(json), Ok(vec![(10, 1)]));
    }

    #[test]
    fn dcb_app_json_rejects_bad_entries() {
        assert!(parse_dcb_app_json("not json").is_err());
        assert!(parse_dcb_app_json(r#"{"dscp_prio":[[26]]}"#).is_err());
        assert!(parse_dcb_app_json(r#"{"dscp_prio":[[300,3]]}"#).is_err());
        assert!(parse_dcb_app_json(r#"{"dscp_prio":[[26,3,"x"]]}"#).is_err());
        assert!(parse_dcb_app_json(r#"{"dscp_prio":[[26,3,4]]}"#).is_err());
        assert!(parse_dcb_app_json(r#"{"dscp_prio":[["26",3]]}"#).is_err());
    }

    #[test]
    fn dcb_app_json_rejects_non_object_top_level() {
        for json in ["null", "[]", "42", r#""{}""#] {
            assert!(parse_dcb_app_json(json).is_err(), "{json}");
        }
    }

    #[test]
    fn dcb_pfc_json_reads_prio_vector() {
        let json = r#"{"pfc_cap":8,"macsec_bypass":false,"delay":7,"prio_pfc":[false,false,false,false,false,false,false,false]}"#;
        assert_eq!(parse_dcb_pfc_json(json), Ok([false; 8]));
        let json = r#"{"prio_pfc":[false,false,false,true,false,false,false,false]}"#;
        assert_eq!(
            parse_dcb_pfc_json(json),
            Ok([false, false, false, true, false, false, false, false])
        );
    }

    #[test]
    fn dcb_pfc_json_rejects_wrong_shape() {
        assert!(parse_dcb_pfc_json("{}").is_err());
        assert!(parse_dcb_pfc_json(r#"{"prio_pfc":[true,false]}"#).is_err());
    }

    #[test]
    fn pause_parses_ethtool_a_output() {
        let out = "Pause parameters for enp65s0np0:\nAutonegotiate:\toff\nRX:\t\ton\nTX:\t\ton\n\n";
        assert_eq!(parse_pause(out), Ok(Pause { rx: true, tx: true }));
        let out = "Pause parameters for enp65s0np0:\nAutonegotiate:\toff\nRX:\t\toff\nTX:\t\toff\n";
        assert_eq!(
            parse_pause(out),
            Ok(Pause {
                rx: false,
                tx: false
            })
        );
    }

    #[test]
    fn pause_ignores_negotiated_lines_and_rejects_missing() {
        let out =
            "Autonegotiate:\ton\nRX:\t\toff\nTX:\t\ton\nRX negotiated: on\nTX negotiated: on\n";
        assert_eq!(
            parse_pause(out),
            Ok(Pause {
                rx: false,
                tx: true
            })
        );
        assert!(parse_pause("Autonegotiate:\toff\n").is_err());
    }

    #[test]
    fn tunable_parses_value() {
        assert_eq!(
            parse_tunable("pfc-prevention-tout: 8000\n", "pfc-prevention-tout"),
            Ok(8000)
        );
        assert!(parse_tunable("pfc-prevention-tout: auto\n", "pfc-prevention-tout").is_err());
        assert!(parse_tunable("rx-copybreak: 256\n", "pfc-prevention-tout").is_err());
    }

    #[test]
    fn tos_parses_configfs_content() {
        assert_eq!(parse_tos("106\n"), Ok(106));
        assert_eq!(parse_tos("0"), Ok(0));
        assert!(parse_tos("x").is_err());
    }
}
