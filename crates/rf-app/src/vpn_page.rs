//! Settings > VPN: the Mullvad account (sign in / out, days left), connect / disconnect, the place (country and city, with
//! search), the fastest server, connect when leaf starts, the kill switch, downloads through the VPN and Mullvad's DNS
//! blocking. Built from the settings page's own cards and switches.

use gpui_kit::assets::IconName as L;
use gpui_kit::*;

use crate::select_box::{Choice, select_box};
use crate::settings::{self, Settings};
use crate::settings_view::{button, card, section, sw};
use crate::t;
use crate::theme::Palette;

pub fn page(p: &Palette, s: &Settings, cx: &mut App) -> Vec<AnyElement> {
    // (the settings search builds every page: that is no reason to open the keyring or ask Mullvad)
    if !crate::settings_view::collecting() {
        crate::vpn::want_account(cx);
        crate::vpn::want_relays(cx);
    }
    let acc = crate::vpn::account_view();
    let busy = crate::vpn::busy();
    let state = leaf_vpn::state();
    let mut v = vec![section(p, t!("Mullvad account"))];

    let (acc_desc, acc_button): (String, AnyElement) = if acc.signed_in {
        let mut d = t!("Signed in with the account {hint}. This PC is the device \"{device}\".").replace("{hint}", &acc.hint).replace("{device}", &acc.device);
        match acc.days_left {
            Some(n) if n < 0 => d.push_str(&format!(" {}", t!("The account has no time left."))),
            Some(n) => d.push_str(&format!(" {}", t!("{n} days left.").replace("{n}", &n.to_string()))),
            None => {}
        }
        if acc.device_gone {
            d = t!("This device was removed from the Mullvad account. Sign out, then sign in again.").to_string();
        }
        (d, button("vpn-sign-out", p, if busy { t!("Working…") } else { t!("Sign out") }, move |_, cx| {
            if !busy {
                crate::vpn::sign_out(cx)
            }
        }))
    } else {
        (
            t!("Sign in with your Mullvad account number. Only leaf's own connections use the VPN: other programs and the rest of the PC are not changed, and no administrator rights are needed.").to_string(),
            button("vpn-sign-in", p, if busy { t!("Working…") } else { t!("Sign in…") }, move |window, cx| {
                if !busy {
                    crate::vpn::sign_in_dialog(window, cx)
                }
            }),
        )
    };
    v.push(card(p, L::ShieldCheck, t!("Account"), Some(&acc_desc), acc_button));
    if let Some(m) = crate::vpn::message().filter(|m| !m.trim().is_empty()) {
        v.push(card(p, L::Info, t!("Last message"), Some(&m), button("vpn-msg-ok", p, t!("OK"), |_, _| crate::vpn::clear_message())));
    }

    let (word, line) = crate::vpn::describe(&state);
    let conn_desc = match line {
        Some(l) => format!("{word}: {l}"),
        None => word.to_string(),
    };
    let running = !matches!(state, leaf_vpn::State::Off | leaf_vpn::State::Failed(_));
    let conn_button = if running {
        button("vpn-disconnect", p, t!("Disconnect"), |_, cx| crate::vpn::disconnect(cx))
    } else {
        button("vpn-connect", p, t!("Connect"), |_, cx| crate::vpn::connect(cx))
    };
    v.push(card(p, L::Power, t!("Connection"), Some(&conn_desc), conn_button));

    // ---------------------------------------------------------------- the place
    v.push(section(p, t!("Server")));
    let relays = crate::vpn::relays();
    let places = leaf_vpn::mullvad::places(&relays);
    let country = s.vpn_country.trim().to_ascii_lowercase();
    let city = s.vpn_city.trim().to_ascii_lowercase();
    let country_name = places.iter().find(|(_, c, _)| *c == country).map(|(n, _, _)| n.clone());
    let country_label = country_name.clone().unwrap_or_else(|| if country.is_empty() { t!("Anywhere").to_string() } else { country.to_ascii_uppercase() });
    let list = places.clone();
    let now = country.clone();
    let country_box = select_box("vpn-country", p, country_label, move |_| {
        let mut c = vec![Choice::new(t!("Anywhere"), now.is_empty(), |_, cx| settings::update(cx, |s| {
            s.vpn_country.clear();
            s.vpn_city.clear();
        }))];
        c.extend(list.iter().map(|(name, code, _)| {
            let code = code.clone();
            Choice::new(name.clone(), code == now, move |_, cx| {
                let code = code.clone();
                settings::update(cx, move |s| {
                    if s.vpn_country != code {
                        s.vpn_city.clear();
                    }
                    s.vpn_country = code;
                })
            })
        }));
        c
    });
    let country_desc = if relays.is_empty() { t!("The list of Mullvad's servers is being read…").to_string() } else { t!("{n} servers in {c} countries").replace("{n}", &relays.len().to_string()).replace("{c}", &places.len().to_string()) };
    v.push(card(p, L::Globe, t!("Country"), Some(&country_desc), country_box));
    if let Some((_, _, cities)) = places.iter().find(|(_, c, _)| *c == country) {
        let city_label = cities.iter().find(|(_, c)| *c == city).map(|(n, _)| n.clone()).unwrap_or_else(|| t!("Any city").to_string());
        let cities = cities.clone();
        let now = city.clone();
        let city_box = select_box("vpn-city", p, city_label, move |_| {
            let mut c = vec![Choice::new(t!("Any city"), now.is_empty(), |_, cx| settings::update(cx, |s| s.vpn_city.clear()))];
            c.extend(cities.iter().map(|(name, code)| {
                let code = code.clone();
                Choice::new(name.clone(), code == now, move |_, cx| {
                    let code = code.clone();
                    settings::update(cx, move |s| s.vpn_city = code)
                })
            }));
            c
        });
        v.push(card(p, L::Globe, t!("City"), None, city_box));
    }
    v.push(card(
        p,
        L::Gauge,
        t!("Fastest server"),
        Some(t!("At each connect, a few servers of the place are measured (one WireGuard handshake each) and the quickest one is used.")),
        sw("vpn-fastest", p, s.vpn_fastest, |s, v| s.vpn_fastest = v),
    ));

    // ---------------------------------------------------------------- behaviour
    v.push(section(p, t!("What uses the VPN")));
    v.push(card(
        p,
        L::Download,
        t!("Downloads through the VPN"),
        Some(t!("New downloads go through the VPN; each one can still say otherwise in Download here…. Addresses on your own network never do.")),
        sw("vpn-downloads", p, s.vpn_downloads, |s, v| s.vpn_downloads = v),
    ));
    v.push(card(
        p,
        L::Server,
        t!("Hosts"),
        Some(t!("An SFTP or FTP host goes through the VPN when \"Through VPN\" is ticked in its host dialog (Settings > Hosts).")),
        button("vpn-hosts", p, t!("Hosts"), |window, cx| {
            crate::settings_view::open_page(crate::settings_view::Page::Hosts, cx);
            window.refresh();
        }),
    ));
    v.push(card(
        p,
        L::Lock,
        t!("Kill switch for leaf"),
        Some(t!("While the VPN is not connected, the downloads and hosts that are set to use it wait instead of going out directly. Other programs are never affected.")),
        sw("vpn-kill", p, s.vpn_kill_switch, |s, v| s.vpn_kill_switch = v),
    ));
    v.push(card(p, L::Power, t!("Connect when leaf starts"), None, sw("vpn-on-start", p, s.vpn_connect_on_start, |s, v| s.vpn_connect_on_start = v)));

    // ---------------------------------------------------------------- DNS
    v.push(section(p, t!("DNS blocking")));
    v.push(card(
        p,
        L::ShieldCheck,
        t!("Block ads"),
        Some(t!("Names are looked up through the VPN by Mullvad's own resolver; these lists make it answer \"no such name\" for what they hold.")),
        sw("vpn-dns-ads", p, s.vpn_block_ads, |s, v| s.vpn_block_ads = v),
    ));
    v.push(card(p, L::ShieldCheck, t!("Block trackers"), None, sw("vpn-dns-trackers", p, s.vpn_block_trackers, |s, v| s.vpn_block_trackers = v)));
    v.push(card(p, L::ShieldCheck, t!("Block malware"), None, sw("vpn-dns-malware", p, s.vpn_block_malware, |s, v| s.vpn_block_malware = v)));
    v.push(card(p, L::ShieldCheck, t!("Block adult content"), None, sw("vpn-dns-adult", p, s.vpn_block_adult, |s, v| s.vpn_block_adult = v)));
    v.push(card(p, L::ShieldCheck, t!("Block gambling"), None, sw("vpn-dns-gambling", p, s.vpn_block_gambling, |s, v| s.vpn_block_gambling = v)));
    v.push(card(p, L::ShieldCheck, t!("Block social media"), None, sw("vpn-dns-social", p, s.vpn_block_social, |s, v| s.vpn_block_social = v)));
    v
}
