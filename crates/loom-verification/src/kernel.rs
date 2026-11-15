use anyhow::{Context, Result, ensure};
use regex::Regex;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{self, Write},
    path::Path,
};

type Entry = (String, Vec<u8>);

fn encode(dotted: &str) -> Result<Vec<u8>> {
    let parts = dotted
        .split('.')
        .map(str::parse::<u64>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    ensure!(
        parts.len() >= 2 && parts[0] <= 2 && (parts[0] == 2 || parts[1] < 40),
        "invalid object identifier"
    );
    let first = parts[0]
        .checked_mul(40)
        .and_then(|value| value.checked_add(parts[1]))
        .context("object identifier prefix overflow")?;
    let mut encoded = vec![u8::try_from(first)?];
    for number in &parts[2..] {
        let mut number = *number;
        let mut octets = vec![u8::try_from(number & 127)?];
        number >>= 7;
        while number > 0 {
            octets.push(128 | u8::try_from(number & 127)?);
            number >>= 7;
        }
        encoded.extend(octets.into_iter().rev());
    }
    Ok(encoded)
}

fn lookup_hash(data: &[u8]) -> Result<u8> {
    ensure!(!data.is_empty(), "empty encoded object identifier");
    let value =
        u64::try_from(data.len() - 1)? + data.iter().map(|b| u64::from(*b) * 33).sum::<u64>();
    Ok(u8::try_from(
        ((value >> 24) ^ (value >> 16) ^ (value >> 8) ^ value) & 255,
    )?)
}

fn hex(value: &str) -> Result<Vec<u8>> {
    ensure!(
        value.len().is_multiple_of(2) && value.is_ascii(),
        "invalid hexadecimal object identifier"
    );
    value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| Ok(u8::from_str_radix(std::str::from_utf8(pair)?, 16)?))
        .collect()
}

fn validate(header: &str, generated: &str) -> Result<usize> {
    let pattern = Regex::new(r"\bOID_(\w+),\s*/\*\s*([012][.\d]*)\s*\*/")?;
    let entries = pattern
        .captures_iter(header)
        .map(|c| Ok((c[1].to_owned(), encode(&c[2])?)))
        .collect::<Result<Vec<Entry>>>()?;
    let expected: BTreeMap<_, _> = entries.iter().cloned().collect();
    ensure!(
        !entries.is_empty() && expected.len() == entries.len(),
        "missing or duplicate object identifiers"
    );
    ensure!(
        expected.get("signed_data") == Some(&hex("2a864886f70d010702")?),
        "signed_data encoding differs"
    );
    let block = Regex::new(r"(?s)oid_data\[\d+\] = \{(.*?)\n\};")?
        .captures(generated)
        .context("missing generated OID bytes")?
        .get(1)
        .context("missing OID block")?
        .as_str();
    let mut actual = BTreeMap::new();
    for row in block.trim().lines() {
        let (numbers, name) = row.split_once("//").context("missing OID byte label")?;
        let bytes = numbers
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::parse::<u8>)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        ensure!(
            actual.insert(name.trim().to_owned(), bytes).is_none(),
            "duplicate generated OID label"
        );
    }
    ensure!(actual == expected, "generated OID encoding differs");
    let offsets = Regex::new(r"\[OID_(\w+)\] = (\d+),?")?
        .captures_iter(generated)
        .map(|c| Ok((c[1].to_owned(), c[2].parse::<usize>()?)))
        .collect::<Result<BTreeMap<_, _>>>()?;
    let mut offset = 0;
    for (name, data) in &entries {
        ensure!(
            offsets.get(name) == Some(&offset),
            "incorrect offset: {name}"
        );
        offset += data.len();
    }
    ensure!(
        offsets.get("_NR") == Some(&offset),
        "incorrect final OID offset"
    );
    let mut ordered = entries
        .iter()
        .map(|(name, data)| {
            Ok((
                lookup_hash(data)?,
                data.len(),
                data.iter().rev().copied().collect::<Vec<_>>(),
                name,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    ordered.sort_by(|left, right| (&left.0, &left.1, &left.2).cmp(&(&right.0, &right.1, &right.2)));
    let rows: Vec<_> = Regex::new(r"\[\s*(\d+)\] = \{\s*(\d+), OID_(\w+)\s*\}, // (\w+)")?
        .captures_iter(generated)
        .map(|c| {
            Ok((
                c[1].parse::<usize>()?,
                c[2].parse::<u8>()?,
                c[3].to_owned(),
                hex(&c[4])?,
            ))
        })
        .collect::<Result<_>>()?;
    ensure!(rows.len() == ordered.len(), "OID lookup length differs");
    let mut seen = BTreeSet::new();
    for (index, (position, digest, name, data)) in rows.into_iter().enumerate() {
        ensure!(
            seen.insert(name.clone()) && position == index && &name == ordered[index].3,
            "incorrect OID lookup order"
        );
        ensure!(
            digest == lookup_hash(&expected[&name])? && data == expected[&name],
            "incorrect OID lookup hash or bytes"
        );
    }
    Ok(entries.len())
}

pub fn check(header: &Path, generated: &Path) -> Result<()> {
    let count = validate(
        &fs::read_to_string(header)?,
        &fs::read_to_string(generated)?,
    )?;
    writeln!(
        io::stdout().lock(),
        "validated {count} kernel OID encodings, offsets and lookup hashes"
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_signed_data_and_base128_boundaries() {
        assert_eq!(
            encode("1.2.840.113549.1.7.2").unwrap(),
            hex("2a864886f70d010702").unwrap()
        );
        assert_eq!(
            encode("1.2.0.127.128.16384").unwrap(),
            vec![42, 0, 127, 129, 0, 129, 128, 0]
        );
        assert!(encode("1.40.1").is_err());
        assert!(encode("1").is_err());
        assert!(encode("2.18446744073709551615").is_err());
    }

    #[test]
    fn rejects_corrupt_encoding_offset_and_lookup_independently() {
        let header = "OID_signed_data, /* 1.2.840.113549.1.7.2 */";
        let digest = lookup_hash(&encode("1.2.840.113549.1.7.2").unwrap()).unwrap();
        let generated = format!(
            "oid_data[9] = {{\n42,134,72,134,247,13,1,7,2, // signed_data\n}};\n[OID_signed_data] = 0,\n[OID__NR] = 9,\n[0] = {{ {digest}, OID_signed_data }}, // 2a864886f70d010702"
        );
        assert_eq!(validate(header, &generated).unwrap(), 1);
        for corrupt in [
            generated.replace("42,134", "43,134"),
            generated.replace("= 0,", "= 1,"),
            generated.replace("[0]", "[1]"),
            generated.replace(&format!("{{ {digest},"), "{ 0,"),
        ] {
            assert!(validate(header, &corrupt).is_err());
        }
    }
}
