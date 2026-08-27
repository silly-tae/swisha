//! Reading certificates, and checking a signing pair really is a pair.

use crate::error::{Result, err};

use crate::util::{base64, hex};

// Uppercase hex, no separators: the exact form Swish expects in
// signingCertificateSerialNumber.
/// The leaf certificate's serial number, as Swish expects it in the payload.
///
/// Swish's PEM files are three-certificate chains; only the first is the merchant's own.
pub fn read_cert_serial(cert_pem: &str) -> Result<String> {
    let der = first_certificate_der(cert_pem)?;
    Ok(hex::upper(serial_from_der(&der)?))
}

// Whether a certificate and a private key are actually a pair.
//
// A mismatched pair starts the service perfectly happily and is then rejected by Swish on every
// payout with PA01, "Parameter is not correct", which names no key and sends you hunting. Four
// files with near-identical names make the mixup easy, so it is worth refusing at startup.
//
/// Whether a certificate and a private key are actually a pair.
///
/// Checked at startup, because half a signing pair produces a signature Swish cannot verify and
/// rejects as `PA01` on every payout, with nothing in the message pointing at the cause.
///
/// The certificate embeds the key's PKCS#1 `RSAPublicKey` verbatim, so containment settles it
/// and no X.509 parsing is needed beyond finding the leaf.
pub fn certificate_matches_key(cert_pem: &str, key_pem: &str) -> Result<bool> {
    let certificate = first_certificate_der(cert_pem)?;
    let pair = crate::swish::sign::rsa_key_pair(key_pem)?;

    let public = pair.public().as_ref();
    Ok(certificate.windows(public.len()).any(|window| window == public))
}

// Takes only the first block, so a PEM holding a full chain still yields the leaf.
fn first_certificate_der(pem: &str) -> Result<Vec<u8>> {
    const BEGIN: &str = "-----BEGIN CERTIFICATE-----";
    const END: &str = "-----END CERTIFICATE-----";

    let after_begin = pem
        .find(BEGIN)
        .map(|i| i + BEGIN.len())
        .ok_or_else(|| err("no CERTIFICATE block found in PEM"))?;
    let rest = &pem[after_begin..];
    let end = rest
        .find(END)
        .ok_or_else(|| err("unterminated CERTIFICATE block in PEM"))?;

    let body: String = rest[..end].split_whitespace().collect();
    base64::decode(&body).map_err(|e| err(format!("certificate base64 is invalid: {e}")))
}

// Certificate ::= SEQUENCE { tbsCertificate SEQUENCE { [0] version OPTIONAL,
// serialNumber INTEGER, ... }, ... }. The INTEGER contents are returned verbatim so the
// DER leading-zero pad survives, since that pad is part of the serial Swish expects.
fn serial_from_der(der: &[u8]) -> Result<&[u8]> {
    let (tag, certificate) = read_tlv(der, &mut 0)?;
    if tag != 0x30 {
        return Err(err("certificate is not a DER SEQUENCE"));
    }

    let (tag, tbs) = read_tlv(certificate, &mut 0)?;
    if tag != 0x30 {
        return Err(err("tbsCertificate is not a DER SEQUENCE"));
    }

    let mut pos = 0;
    if tbs.first() == Some(&0xa0) {
        read_tlv(tbs, &mut pos)?;
    }

    let (tag, serial) = read_tlv(tbs, &mut pos)?;
    if tag != 0x02 {
        return Err(err("serialNumber is not a DER INTEGER"));
    }
    Ok(serial)
}

fn read_tlv<'a>(buf: &'a [u8], pos: &mut usize) -> Result<(u8, &'a [u8])> {
    let tag = *buf.get(*pos).ok_or_else(|| err("truncated DER: missing tag"))?;
    *pos += 1;

    let len = read_length(buf, pos)?;
    let start = *pos;
    let end = start
        .checked_add(len)
        .ok_or_else(|| err("DER length overflows"))?;
    if end > buf.len() {
        return Err(err("DER length runs past the end of the buffer"));
    }

    *pos = end;
    Ok((tag, &buf[start..end]))
}

fn read_length(buf: &[u8], pos: &mut usize) -> Result<usize> {
    let first = *buf.get(*pos).ok_or_else(|| err("truncated DER: missing length"))?;
    *pos += 1;
    if first < 0x80 {
        return Ok(first as usize);
    }

    let count = (first & 0x7f) as usize;
    if count == 0 || count > 4 {
        return Err(err("unsupported DER length encoding"));
    }

    let mut len = 0usize;
    for _ in 0..count {
        let b = *buf.get(*pos).ok_or_else(|| err("truncated DER: short length"))?;
        *pos += 1;
        len = (len << 8) | b as usize;
    }
    Ok(len)
}

