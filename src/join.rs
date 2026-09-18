//! Getting a key, without a person editing a file by hand, and without a
//! stranger being able to ask for one.
//!
//! A key is a name, and a name has to be agreed. So the question is only
//! *how* agreement is shown, and there are exactly two honest answers here:
//!
//!   1. **You are already on the machine.** Then you can read
//!      `~/.config/kapwa/enroll`, a file only its owner can read — and you
//!      could equally have read the keys next to it. Presenting it proves
//!      nothing new, which is the point: it grants no access that running
//!      as that user did not already grant. It only lets you have a name of
//!      your own instead of sharing one.
//!   2. **Somebody who is already here vouched for you.** They mint an
//!      invitation: one use, an expiry, a role and topics fixed at the
//!      moment of vouching. That is what a remote agent redeems.
//!
//! Anything else is refused. The open front door explains both and hands
//! out neither, so a stranger who finds the URL learns how the mesh works
//! and still cannot join it.
//!
//! Deliberately *not* done by reading `X-Forwarded-For` or the peer address
//! to decide "is this local": every node sits behind a tunnel that connects
//! to loopback, so the socket says nothing, and headers say whatever the
//! last hop wants them to. A secret in a file is the same trust boundary
//! stated in a way nothing on the wire can forge.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::PathBuf;

use constant_time_eq::constant_time_eq;

use crate::config::Config;

/// Random, from the system. No dependency, and no cleverness to get wrong.
pub fn secret(bytes: usize) -> String {
    let mut b = vec![0u8; bytes];
    File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut b))
        .expect("/dev/urandom");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn dir(cfg: &Config) -> PathBuf {
    cfg.agents_file
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn private(path: &PathBuf, line: &str) -> std::io::Result<()> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    writeln!(f, "{line}")
}

/// The proof that you are on this machine: made once, readable only by the
/// user the node runs as.
pub fn enroll_secret(cfg: &Config) -> String {
    let p = dir(cfg).join("enroll");
    if let Ok(s) = std::fs::read_to_string(&p) {
        let s = s.trim().to_string();
        if !s.is_empty() {
            return s;
        }
    }
    let s = secret(32);
    let _ = private(&p, &s);
    s
}

pub fn is_local(cfg: &Config, given: &str) -> bool {
    !given.is_empty() && constant_time_eq(enroll_secret(cfg).as_bytes(), given.as_bytes())
}

#[derive(Clone, Debug)]
pub struct Invite {
    pub token: String,
    pub name: String,
    pub role: String,
    pub topics: String,
    pub until: String,
}

impl Invite {
    fn line(&self) -> String {
        format!(
            "{}:{}:{}:{}:{}",
            self.token, self.name, self.role, self.topics, self.until
        )
    }
    fn parse(l: &str) -> Option<Invite> {
        let mut p = l.trim().splitn(5, ':');
        Some(Invite {
            token: p.next()?.into(),
            name: p.next()?.into(),
            role: p.next()?.into(),
            topics: p.next()?.into(),
            until: p.next()?.into(),
        })
    }
}

/// Vouch for somebody: one use, and it stops working on its own.
pub fn mint_invite(cfg: &Config, name: &str, role: &str, topics: &str, hours: i64) -> Invite {
    let inv = Invite {
        token: secret(24),
        name: name.to_string(),
        role: role.to_string(),
        topics: topics.to_string(),
        until: (chrono::Utc::now() + chrono::Duration::hours(hours.clamp(1, 24 * 14)))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    };
    let _ = private(&dir(cfg).join("invites"), &inv.line());
    inv
}

/// Spend an invitation. Removing it before the key is made is deliberate:
/// two agents racing the same token must not both get in, and a token that
/// went missing mid-use is a small loss beside a token used twice.
pub fn redeem(cfg: &Config, token: &str) -> Option<Invite> {
    let p = dir(cfg).join("invites");
    let body = std::fs::read_to_string(&p).ok()?;
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut found = None;
    let mut keep: Vec<String> = vec![];
    for l in body.lines().filter(|l| !l.trim().is_empty()) {
        match Invite::parse(l) {
            // expired ones are swept while we are here
            Some(i) if i.until <= now => {}
            Some(i) if constant_time_eq(i.token.as_bytes(), token.as_bytes()) => found = Some(i),
            _ => keep.push(l.to_string()),
        }
    }
    // rewrite whenever anything went, spent or expired: sweeping only on a
    // hit would let dead invitations pile up forever in a file nobody reads
    if keep.len() != body.lines().filter(|l| !l.trim().is_empty()).count() {
        let _ = std::fs::write(
            &p,
            keep.join("\n") + if keep.is_empty() { "" } else { "\n" },
        );
    }
    found
}

pub fn taken(cfg: &Config, name: &str) -> bool {
    std::fs::read_to_string(&cfg.agents_file)
        .map(|b| {
            b.lines()
                .any(|l| l.split(':').next().map(str::trim) == Some(name))
        })
        .unwrap_or(false)
}

/// Write the key down where the node reads keys. Returns the token, which
/// is the only time it is ever known: nothing stores it but the agents file
/// and whoever asked.
pub fn add_key(cfg: &Config, name: &str, role: &str, topics: &str) -> std::io::Result<String> {
    let token = secret(24);
    private(&cfg.agents_file, &format!("{name}:{token}:{role}:{topics}"))?;
    Ok(token)
}

/// Give an existing agent a new token, keeping its name, role and topics.
/// The old one stops working the moment this returns, because the file is
/// read per request — there is no window where both are good.
///
/// Rewritten whole rather than edited in place: the file is small, and a
/// half-written credential file locks everyone out of the node at once.
pub fn rotate_key(cfg: &Config, name: &str) -> std::io::Result<String> {
    let body = std::fs::read_to_string(&cfg.agents_file)?;
    let token = secret(24);
    let mut found = false;
    let out: Vec<String> = body
        .lines()
        .map(|l| {
            let mut p: Vec<&str> = l.split(':').collect();
            if l.trim_start().starts_with('#') || p.len() < 2 || p[0].trim() != name {
                return l.to_string();
            }
            found = true;
            p[1] = &token;
            p.join(":")
        })
        .collect();
    if !found {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("no agent called `{name}` here"),
        ));
    }
    let tmp = cfg.agents_file.with_extension("rotating");
    {
        let mut f = File::create(&tmp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
        }
        writeln!(f, "{}", out.join("\n"))?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &cfg.agents_file)?;
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(tmp: &std::path::Path) -> Config {
        Config {
            private_topics: vec![],
            writer: "t".into(),
            dir: tmp.join("data"),
            port: 0,
            peers: vec![],
            mesh_token: None,
            agents_file: tmp.join("agents"),
            public_url: String::new(),
            secret_key_base: None,
            oidc: None,
        }
    }

    #[test]
    fn the_enrollment_secret_is_made_once_and_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let c = cfg(tmp.path());
        let a = enroll_secret(&c);
        assert_eq!(a.len(), 64);
        assert_eq!(a, enroll_secret(&c), "asking twice must not change it");
        assert!(is_local(&c, &a));
        assert!(!is_local(&c, "guess"));
        assert!(!is_local(&c, ""), "an empty secret must never pass");
    }

    #[test]
    fn an_invitation_works_once() {
        let tmp = tempfile::tempdir().unwrap();
        let c = cfg(tmp.path());
        let i = mint_invite(&c, "grokbot", "lead", "a2p", 2);
        assert!(redeem(&c, "wrong").is_none());
        let got = redeem(&c, &i.token).expect("first use");
        assert_eq!((got.name.as_str(), got.role.as_str()), ("grokbot", "lead"));
        assert!(redeem(&c, &i.token).is_none(), "a second use must fail");
    }

    #[test]
    fn an_expired_invitation_is_no_invitation() {
        let tmp = tempfile::tempdir().unwrap();
        let c = cfg(tmp.path());
        let mut i = mint_invite(&c, "late", "worker", "*", 1);
        i.until = "2020-01-01T00:00:00Z".into();
        std::fs::write(tmp.path().join("invites"), i.line() + "\n").unwrap();
        assert!(redeem(&c, &i.token).is_none());
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("invites"))
                .unwrap()
                .trim(),
            "",
            "and it is swept"
        );
    }

    #[test]
    fn rotating_replaces_one_token_and_disturbs_nothing_else() {
        let tmp = tempfile::tempdir().unwrap();
        let c = cfg(tmp.path());
        std::fs::write(
            &c.agents_file,
            "# keys\nana:ana-key:lead:*\nbob:bob-key:worker:a2p,roof\ncar:car-key:worker:*\n",
        )
        .unwrap();
        let fresh = rotate_key(&c, "bob").unwrap();
        let body = std::fs::read_to_string(&c.agents_file).unwrap();
        // the old one is gone, the new one is there, and it is still bob
        assert!(!body.contains("bob-key"), "the old token must not survive");
        assert!(body.contains(&format!("bob:{fresh}:worker:a2p,roof")));
        // nobody else moved, and the comment stayed
        assert!(body.contains("# keys"));
        assert!(body.contains("ana:ana-key:lead:*") && body.contains("car:car-key:worker:*"));
        // rotating twice never gives the same token back
        assert_ne!(fresh, rotate_key(&c, "bob").unwrap());
        // and a name that is not here is an error, not a silent no-op
        assert!(rotate_key(&c, "nobody").is_err());
        assert!(!std::fs::read_to_string(&c.agents_file)
            .unwrap()
            .contains("nobody"));
    }

    #[test]
    fn a_name_is_taken_once_somebody_has_it() {
        let tmp = tempfile::tempdir().unwrap();
        let c = cfg(tmp.path());
        assert!(!taken(&c, "grokbot"));
        add_key(&c, "grokbot", "lead", "a2p").unwrap();
        assert!(taken(&c, "grokbot"));
        let body = std::fs::read_to_string(&c.agents_file).unwrap();
        assert!(body.starts_with("grokbot:") && body.trim().ends_with(":lead:a2p"));
    }
}
