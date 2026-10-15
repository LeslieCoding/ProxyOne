use clash_verge_logging::{Type, logging};

use super::{
    seq::{SeqMap, use_seq},
    use_lowercase,
};
use serde_yaml_ng::{self, Mapping, Value};

/// Merge 文件里带语义的序列键:`prepend-rules` 等不是普通配置键,
/// 深合并会把它们原样塞进配置(内核不识别,规则永远不生效),
/// 必须翻译成对目标数组的前插/后插。
const SEQ_KEYS: &[(&str, &str, bool)] = &[
    ("prepend-rules", "rules", true),
    ("append-rules", "rules", false),
    ("prepend-proxies", "proxies", true),
    ("append-proxies", "proxies", false),
    ("prepend-proxy-groups", "proxy-groups", true),
    ("append-proxy-groups", "proxy-groups", false),
];

fn deep_merge(a: &mut Value, b: Value) {
    match (a, b) {
        (Value::Mapping(a_map), Value::Mapping(b_map)) => {
            for (key, value) in b_map {
                if let Some(existing) = a_map.get_mut(&key) {
                    deep_merge(existing, value);
                } else {
                    a_map.insert(key, value);
                }
            }
        }
        (a, b) => *a = b,
    }
}

pub fn use_merge(merge: &Mapping, config: Mapping) -> Mapping {
    let mut merge = use_lowercase(merge);

    let mut seq_maps: Vec<(&str, SeqMap)> = Vec::new();
    for &(special_key, field, is_prepend) in SEQ_KEYS {
        if let Some(value) = merge.remove(special_key) {
            let Some(seq) = value.as_sequence().cloned() else {
                logging!(warn, Type::Core, "merge key `{special_key}` is not a sequence, ignored");
                continue;
            };
            if seq.is_empty() {
                continue;
            }
            let entry = seq_maps.iter_mut().find(|(f, _)| *f == field);
            match (entry, is_prepend) {
                (Some((_, map)), true) => map.prepend = seq,
                (Some((_, map)), false) => map.append = seq,
                (None, true) => seq_maps.push((
                    field,
                    SeqMap {
                        prepend: seq,
                        ..Default::default()
                    },
                )),
                (None, false) => seq_maps.push((
                    field,
                    SeqMap {
                        append: seq,
                        ..Default::default()
                    },
                )),
            }
        }
    }

    let mut config = Value::from(config);
    deep_merge(&mut config, Value::from(merge));
    let mut config = config.as_mapping().cloned().unwrap_or_else(|| {
        logging!(
            error,
            Type::Core,
            "Failed to convert merged config to mapping, using empty mapping"
        );
        Mapping::new()
    });

    for (field, seq_map) in seq_maps {
        config = use_seq(seq_map, config, field);
    }
    config
}

#[test]
fn test_merge() -> anyhow::Result<()> {
    let merge = r"
    prepend-rules:
      - prepend
      - '1123123'
    append-rules:
      - append
    prepend-proxies:
      - prepend-proxy
    append-proxies:
      - append-proxy
    rules:
      - replace
    proxy-groups: 
      - 123781923810
    tun:
      enable: true
    dns:
      enable: true
  ";

    let config = r"
    rules:
      - aaaaa
    script1: test
  ";

    let merge = serde_yaml_ng::from_str::<Mapping>(merge)?;
    let config = serde_yaml_ng::from_str::<Mapping>(config)?;

    let result = use_merge(&merge, config);

    // 序列语义键必须作用于目标数组,而不是作为顶层键塞进配置
    assert!(!result.contains_key("prepend-rules"));
    assert!(!result.contains_key("append-rules"));
    assert!(!result.contains_key("prepend-proxies"));
    assert!(!result.contains_key("append-proxies"));

    let rules: Vec<_> = result
        .get("rules")
        .and_then(Value::as_sequence)
        .map(|seq| seq.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    assert_eq!(rules, vec!["prepend", "1123123", "replace", "append"]);

    let proxies: Vec<_> = result
        .get("proxies")
        .and_then(Value::as_sequence)
        .map(|seq| seq.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    assert_eq!(proxies, vec!["prepend-proxy", "append-proxy"]);

    // 同名普通键仍然覆盖,非冲突键照常合并
    assert_eq!(
        result.get("proxy-groups").and_then(Value::as_sequence).map(|s| s.len()),
        Some(1)
    );
    assert_eq!(result.get("script1").and_then(Value::as_str), Some("test"));
    assert_eq!(
        result.get("tun").and_then(|v| v.get("enable")).and_then(Value::as_bool),
        Some(true)
    );

    Ok(())
}
