// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

// SSRF classifier battery — proves wheel_core::ip_is_denied / host_is_denied by RUNNING,
// with emphasis on the tricky IPv6-embedded-IPv4 forms (6to4, NAT64, Teredo, mapped, compatible)
// carrying a private or metadata address. Exit non-zero if any expectation fails.
use std::net::{IpAddr, Ipv6Addr};
use wheel_core::{host_is_denied, ip_is_denied};

fn ip(s: &str) -> IpAddr { s.parse().unwrap() }

fn main() {
    let mut fails = 0;
    // (label, addr, must_be_denied)
    let cases: Vec<(&str, IpAddr, bool)> = vec![
        // direct private / special — must be DENIED
        ("loopback v4", ip("127.0.0.1"), true),
        ("rfc1918 10", ip("10.0.0.1"), true),
        ("rfc1918 172.16", ip("172.16.0.1"), true),
        ("rfc1918 192.168", ip("192.168.1.1"), true),
        ("link-local / cloud metadata", ip("169.254.169.254"), true),
        ("CGNAT 100.64", ip("100.64.0.1"), true),
        ("unspecified 0.0.0.0", ip("0.0.0.0"), true),
        ("0/8", ip("0.1.2.3"), true),
        ("reserved 240/4", ip("240.0.0.1"), true),
        ("benchmarking 198.18", ip("198.18.0.1"), true),
        ("loopback v6", ip("::1"), true),
        ("link-local v6", ip("fe80::1"), true),
        ("unique-local fc00", ip("fc00::1"), true),
        ("unique-local fd00", ip("fd00::1"), true),
        // IPv6-embedded IPv4 — the SSRF bypass class — must be DENIED
        ("mapped ::ffff:metadata", IpAddr::V6("::ffff:169.254.169.254".parse().unwrap()), true),
        ("mapped ::ffff:loopback", IpAddr::V6("::ffff:127.0.0.1".parse().unwrap()), true),
        ("compatible ::127.0.0.1", IpAddr::V6("::127.0.0.1".parse().unwrap()), true),
        ("6to4 -> 127.0.0.1", IpAddr::V6(Ipv6Addr::new(0x2002,0x7f00,0x0001,0,0,0,0,0)), true),
        ("6to4 -> metadata", IpAddr::V6(Ipv6Addr::new(0x2002,0xa9fe,0xa9fe,0,0,0,0,0)), true),
        ("NAT64 -> 127.0.0.1", IpAddr::V6(Ipv6Addr::new(0x0064,0xff9b,0,0,0,0,0x7f00,0x0001)), true),
        ("NAT64 -> metadata", IpAddr::V6(Ipv6Addr::new(0x0064,0xff9b,0,0,0,0,0xa9fe,0xa9fe)), true),
        // Teredo client is the last 32 bits XOR all-ones; embed 127.0.0.1 -> !0x7f00,!0x0001
        ("Teredo client -> 127.0.0.1", IpAddr::V6(Ipv6Addr::new(0x2001,0,0,0,0,0,0x80ff,0xfffe)), true),
        ("Teredo client -> metadata", IpAddr::V6(Ipv6Addr::new(0x2001,0,0,0,0,0,!0xa9feu16,!0xa9feu16)), true),
        // genuinely public — must be ALLOWED (not denied)
        ("public 1.1.1.1", ip("1.1.1.1"), false),
        ("public 8.8.8.8", ip("8.8.8.8"), false),
        ("public v6 2606:4700::", ip("2606:4700::1111"), false),
    ];
    for (label, addr, want_denied) in cases {
        let got = ip_is_denied(addr);
        let ok = got == want_denied;
        if !ok { fails += 1; }
        println!("{} ip_is_denied({}) = {} (want denied={}) {}",
            if ok {"PASS"} else {"**FAIL**"}, addr, got, want_denied, label);
    }
    // host_is_denied pre-DNS denylist
    let hosts: Vec<(&str, bool)> = vec![
        ("postgres.railway.internal", true),
        ("host.internal", true),
        ("127.0.0.1", true),
        ("[::1]", true),
        ("10.0.0.1", true),
        ("169.254.169.254", true),
        ("api.example.com", false),
    ];
    for (h, want) in hosts {
        let got = host_is_denied(h);
        let ok = got == want;
        if !ok { fails += 1; }
        println!("{} host_is_denied({}) = {} (want {}) ", if ok {"PASS"} else {"**FAIL**"}, h, got, want);
    }
    if fails == 0 { println!("\nALL PASS — classifier denies every private/metadata/embedded form and allows public."); }
    else { eprintln!("\n{fails} FAILURES — SSRF bypass", ); std::process::exit(1); }
}
