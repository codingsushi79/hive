//! Agent Skills bundled into the binary, installable into Claude Code so any
//! session (or hive agent) knows how to run, talk to, and economise a hive.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

pub struct Skill {
    pub name: &'static str,
    pub summary: &'static str,
    pub body: &'static str,
}

pub const SKILLS: &[Skill] = &[
    Skill {
        name: "hive-orchestrate",
        summary: "spawn, fan out, wait for, review and merge parallel agents",
        body: include_str!("../skills/hive-orchestrate/SKILL.md"),
    },
    Skill {
        name: "hive-comms",
        summary: "messages, the shared board, and reporting between agents",
        body: include_str!("../skills/hive-comms/SKILL.md"),
    },
    Skill {
        name: "hive-lean",
        summary: "keep multi-agent work cheap: models, budgets, briefs, caching",
        body: include_str!("../skills/hive-lean/SKILL.md"),
    },
];

/// `~/.claude/skills`, or `<project>/.claude/skills`.
pub fn target_dir(project: Option<&Path>) -> Result<PathBuf> {
    Ok(match project {
        Some(dir) => dir.join(".claude").join("skills"),
        None => dirs::home_dir()
            .context("no home directory")?
            .join(".claude")
            .join("skills"),
    })
}

pub fn find(name: &str) -> Result<&'static Skill> {
    SKILLS
        .iter()
        .find(|s| s.name == name)
        .ok_or_else(|| anyhow::anyhow!("no bundled skill named `{name}`; see `hive skills list`"))
}

/// Install (or update) skills. Returns one line per skill.
pub fn install(dir: &Path, names: &[String]) -> Result<Vec<String>> {
    let chosen: Vec<&Skill> = if names.is_empty() {
        SKILLS.iter().collect()
    } else {
        names.iter().map(|n| find(n)).collect::<Result<_>>()?
    };
    let mut out = vec![];
    for skill in chosen {
        let path = dir.join(skill.name).join("SKILL.md");
        let verb = match std::fs::read_to_string(&path) {
            Ok(existing) if existing == skill.body => "unchanged",
            Ok(_) => "updated",
            Err(_) => "installed",
        };
        std::fs::create_dir_all(path.parent().unwrap())
            .with_context(|| format!("creating {}", path.display()))?;
        std::fs::write(&path, skill.body).with_context(|| format!("writing {}", path.display()))?;
        out.push(format!("{verb:<9} {}  →  {}", skill.name, path.display()));
    }
    Ok(out)
}

pub fn uninstall(dir: &Path, names: &[String]) -> Result<Vec<String>> {
    let chosen: Vec<&Skill> = if names.is_empty() {
        SKILLS.iter().collect()
    } else {
        names.iter().map(|n| find(n)).collect::<Result<_>>()?
    };
    let mut out = vec![];
    for skill in chosen {
        let folder = dir.join(skill.name);
        let file = folder.join("SKILL.md");
        if !file.exists() {
            out.push(format!("absent    {}", skill.name));
            continue;
        }
        if std::fs::read_to_string(&file)? != skill.body {
            bail!(
                "{} was edited since it was installed; remove it by hand if you mean to",
                file.display()
            );
        }
        std::fs::remove_file(&file)?;
        // Only remove the folder if nothing else was put in it.
        let _ = std::fs::remove_dir(&folder);
        out.push(format!("removed   {}", skill.name));
    }
    Ok(out)
}

pub fn status(dir: &Path) -> Vec<(&'static Skill, &'static str)> {
    SKILLS
        .iter()
        .map(|s| {
            let state = match std::fs::read_to_string(dir.join(s.name).join("SKILL.md")) {
                Ok(t) if t == s.body => "installed",
                Ok(_) => "outdated",
                Err(_) => "-",
            };
            (s, state)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_skills_have_valid_frontmatter() {
        for s in SKILLS {
            assert!(s.body.starts_with("---\n"), "{}", s.name);
            let end = s.body[4..].find("\n---").expect("closing ---");
            let front = &s.body[4..4 + end];
            assert!(front.contains(&format!("name: {}", s.name)), "{}", s.name);
            assert!(front.contains("description:"), "{}", s.name);
        }
    }

    #[test]
    fn install_update_uninstall() {
        let dir = std::env::temp_dir().join(format!("hive-skills-{}", std::process::id()));
        let lines = install(&dir, &[]).unwrap();
        assert!(lines.iter().all(|l| l.starts_with("installed")));
        assert!(install(&dir, &[]).unwrap().iter().all(|l| l.starts_with("unchanged")));
        assert!(status(&dir).iter().all(|(_, st)| *st == "installed"));
        std::fs::write(dir.join("hive-lean/SKILL.md"), "edited").unwrap();
        assert!(uninstall(&dir, &["hive-lean".into()]).is_err());
        uninstall(&dir, &["hive-comms".into()]).unwrap();
        assert!(!dir.join("hive-comms").exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}
