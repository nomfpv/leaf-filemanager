//! The Mullvad account: the number the user types once, this PC's device on it (a new WireGuard key registered with
//! Mullvad), the time left, and signing out (the device is removed from the account). The account number and the key are
//! secrets: their `Debug` shows nothing, they are wiped when dropped, and no error text carries them. Where they are kept
//! is the caller's (leaf keeps them in the system keyring and nowhere else).

use std::net::Ipv4Addr;

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::mullvad::{ApiError, Device, api};
use crate::tunnel::PrivateKey;

/// A Mullvad account number (16 digits). Its `Debug` and `Display` show only that it is one.
#[derive(Clone, PartialEq, Eq)]
pub struct AccountNumber(Zeroizing<String>);

impl AccountNumber {
    /// The number in what the user typed (spaces and dashes between the digits are fine); `None` for anything else.
    pub fn parse(text: &str) -> Option<AccountNumber> {
        let digits: Zeroizing<String> = Zeroizing::new(text.chars().filter(|c| !matches!(c, ' ' | '-' | '\u{a0}')).collect());
        (digits.len() == 16 && digits.bytes().all(|b| b.is_ascii_digit())).then(|| AccountNumber(digits))
    }
    /// The digits (for the keyring and the one API call that takes them).
    pub fn expose(&self) -> &str {
        &self.0
    }
    /// The last four digits, as Mullvad's own app shows a signed-in account ("…1234").
    pub fn hint(&self) -> String {
        format!("…{}", &self.0[self.0.len() - 4..])
    }
}

impl std::fmt::Debug for AccountNumber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AccountNumber(hidden)")
    }
}

/// A short-lived access token (an hour); never logged.
struct Token(Zeroizing<String>);

async fn token(account: &AccountNumber) -> Result<Token, ApiError> {
    #[derive(Serialize)]
    struct Req<'a> {
        account_number: &'a str,
    }
    #[derive(Deserialize)]
    struct Resp {
        access_token: String,
    }
    let body = Zeroizing::new(serde_json::to_vec(&Req { account_number: account.expose() }).map_err(|e| ApiError::Network(e.to_string()))?);
    let a = api("POST", "/auth/v1/token", None, Some(&body)).await?;
    if a.status != 200 {
        return Err(ApiError::from_answer(a.status, &a.body));
    }
    let r: Resp = serde_json::from_slice(&a.body).map_err(|e| ApiError::Network(format!("an answer leaf cannot read ({e})")))?;
    Ok(Token(Zeroizing::new(r.access_token)))
}

/// What signing in gives: this PC's device and the account's end date.
#[derive(Debug, Clone)]
pub struct SignedIn {
    pub device: Device,
    /// RFC 3339 (`2027-01-31T12:00:00+00:00`).
    pub expiry: Option<String>,
}

/// Registers a new key for this PC on the account (Mullvad allows five devices; a sixth is [`ApiError::MaxDevices`]).
pub async fn sign_in(account: &AccountNumber) -> Result<SignedIn, ApiError> {
    let t = token(account).await?;
    let key = PrivateKey::generate();
    #[derive(Serialize)]
    struct Req {
        pubkey: String,
        hijack_dns: bool,
    }
    #[derive(Deserialize)]
    struct Resp {
        id: String,
        name: String,
        ipv4_address: String,
    }
    let body = serde_json::to_vec(&Req { pubkey: key.public_base64(), hijack_dns: false }).map_err(|e| ApiError::Network(e.to_string()))?;
    let a = api("POST", "/accounts/v1/devices", Some(&t.0), Some(&body)).await?;
    if a.status != 201 && a.status != 200 {
        return Err(ApiError::from_answer(a.status, &a.body));
    }
    let r: Resp = serde_json::from_slice(&a.body).map_err(|e| ApiError::Network(format!("an answer leaf cannot read ({e})")))?;
    let ipv4: Ipv4Addr = r.ipv4_address.split('/').next().unwrap_or_default().parse().map_err(|_| ApiError::Network("the device's address cannot be read".into()))?;
    let device = Device { id: r.id, name: r.name, ipv4, key };
    let expiry = expiry_with(&t).await.ok();
    Ok(SignedIn { device, expiry })
}

async fn expiry_with(t: &Token) -> Result<String, ApiError> {
    #[derive(Deserialize)]
    struct Resp {
        expiry: String,
    }
    let a = api("GET", "/accounts/v1/accounts/me", Some(&t.0), None).await?;
    if a.status != 200 {
        return Err(ApiError::from_answer(a.status, &a.body));
    }
    let r: Resp = serde_json::from_slice(&a.body).map_err(|e| ApiError::Network(format!("an answer leaf cannot read ({e})")))?;
    Ok(r.expiry)
}

/// The account's end date now (RFC 3339), and whether this device is still on it (it may have been removed on the website).
pub async fn status(account: &AccountNumber, device_id: &str) -> Result<(String, bool), ApiError> {
    let t = token(account).await?;
    let expiry = expiry_with(&t).await?;
    let a = api("GET", &format!("/accounts/v1/devices/{}", url_part(device_id)), Some(&t.0), None).await?;
    let present = match a.status {
        200 => true,
        404 => false,
        s => return Err(ApiError::from_answer(s, &a.body)),
    };
    Ok((expiry, present))
}

/// Removes this PC's device from the account (gone already counts as done).
pub async fn sign_out(account: &AccountNumber, device_id: &str) -> Result<(), ApiError> {
    let t = token(account).await?;
    let a = api("DELETE", &format!("/accounts/v1/devices/{}", url_part(device_id)), Some(&t.0), None).await?;
    match a.status {
        200 | 204 | 404 => Ok(()),
        s => Err(ApiError::from_answer(s, &a.body)),
    }
}

/// A device id in a path: Mullvad's ids are UUIDs; anything else is cut to what an id can hold.
fn url_part(id: &str) -> String {
    id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect()
}

/// Days from now until `expiry` (RFC 3339, UTC or with an offset), rounded down; negative when it has passed. `None` when
/// it cannot be read.
pub fn days_left(expiry: &str, now_unix: i64) -> Option<i64> {
    let t = parse_rfc3339(expiry)?;
    Some((t - now_unix).div_euclid(86_400))
}

fn parse_rfc3339(s: &str) -> Option<i64> {
    let s = s.trim();
    let (date, rest) = s.split_once(['T', ' '])?;
    let mut d = date.split('-');
    let (y, m, day): (i64, i64, i64) = (d.next()?.parse().ok()?, d.next()?.parse().ok()?, d.next()?.parse().ok()?);
    let time_end = rest.find(['Z', 'z', '+', '-']).unwrap_or(rest.len());
    let (time, zone) = rest.split_at(time_end);
    let mut t = time.split(':');
    let (hh, mm): (i64, i64) = (t.next()?.parse().ok()?, t.next()?.parse().ok()?);
    let ss: i64 = t.next().and_then(|x| x.split('.').next()).and_then(|x| x.parse().ok()).unwrap_or(0);
    let offset = match zone.chars().next() {
        Some(sign @ ('+' | '-')) => {
            let (oh, om) = zone[1..].split_once(':').unwrap_or((&zone[1..], "0"));
            let o = oh.parse::<i64>().ok()? * 3600 + om.parse::<i64>().ok()? * 60;
            if sign == '+' { o } else { -o }
        }
        _ => 0,
    };
    // days from the civil date (Howard Hinnant's algorithm)
    let (y, m) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hh * 3600 + mm * 60 + ss - offset)
}

// ------------------------------------------------------------------------------------------------ what is kept

/// The device as one text for the keyring (its key, id, name and address); [`Device::from_kept`] reads it back.
impl Device {
    pub fn to_kept(&self) -> Zeroizing<String> {
        // (written by hand so no copy of the key is left in a JSON value; base64 needs no escaping)
        let q = |s: &str| serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into());
        let key = self.key.to_base64();
        let mut out = Zeroizing::new(String::with_capacity(160));
        out.push_str(&format!("{{\"id\":{},\"name\":{},\"ipv4\":\"{}\",\"key\":\"", q(&self.id), q(&self.name), self.ipv4));
        out.push_str(&key);
        out.push_str("\"}");
        out
    }

    pub fn from_kept(text: &str) -> Option<Device> {
        #[derive(Deserialize)]
        struct Kept {
            id: String,
            name: String,
            ipv4: Ipv4Addr,
            key: String,
        }
        let mut k: Kept = serde_json::from_str(text).ok()?;
        let key = PrivateKey::from_base64(&k.key);
        zeroize::Zeroize::zeroize(&mut k.key);
        Some(Device { id: k.id, name: k.name, ipv4: k.ipv4, key: key? })
    }
}
