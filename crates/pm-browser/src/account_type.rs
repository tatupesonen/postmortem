//! Web account roles from visited URLs (Autopsy `ExtractWebAccountType`):
//! forum/CMS admin, moderator and user pages, and plain service accounts.

use std::collections::HashMap;

/// A role on a web platform, ordered by rank.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
    User = 0,
    Moderator = 1,
    Administrator = 2,
}

impl Role {
    /// Autopsy's description (`ExtractWebAccountType.role.*`).
    pub fn description(self) -> &'static str {
        match self {
            Self::User => "User role",
            Self::Moderator => "Moderator role",
            Self::Administrator => "Administrator role",
        }
    }
}

/// The roles a URL reveals: `(platform, role)` for each platform rule that
/// matches (`url` must be lower-cased). A service account (saved login)
/// without a platform rule is a user of its own domain.
pub fn roles_for_url(url: &str, domain: &str, service_account: bool) -> Vec<(String, Role)> {
    let mut out = Vec::new();
    let mybb = if url.contains("/admin/index.php") {
        Some(Role::Administrator)
    } else if url.contains("/modcp.php") {
        Some(Role::Moderator)
    } else if url.contains("/usercp.php") {
        Some(Role::User)
    } else {
        None
    };
    if let Some(r) = mybb {
        out.push(("myBB platform".to_owned(), r));
    }
    let phpbb = if url.contains("/adm/index.php") {
        Some(Role::Administrator)
    } else if url.contains("/mcp.php") {
        Some(Role::Moderator)
    } else if url.contains("/ucp.php") {
        Some(Role::User)
    } else {
        None
    };
    if let Some(r) = phpbb {
        out.push(("phpBB platform".to_owned(), r));
    }
    if url.contains("/administrator/index.php") {
        out.push(("Joomla platform".to_owned(), Role::Administrator));
    }
    if url.contains("/wp-admin/") {
        let r = if url.ends_with("/wp-admin/")
            || url.contains("/wp-admin/index.php")
            || url.contains("/wp-admin/profile.php")
        {
            Role::User
        } else {
            Role::Administrator
        };
        out.push(("WordPress platform".to_owned(), r));
    }
    if out.is_empty() && service_account {
        out.push((domain.to_owned(), Role::User));
    }
    out
}

/// The winning role per `(domain, platform)`: the highest rank, the first
/// seen on ties (Autopsy `RoleProcessor`). `T` identifies the source.
#[derive(Debug)]
pub struct RoleProcessor<T> {
    roles: HashMap<(String, String), (Role, String, T)>,
    order: Vec<(String, String)>,
}

impl<T> Default for RoleProcessor<T> {
    fn default() -> Self {
        Self {
            roles: HashMap::new(),
            order: Vec::new(),
        }
    }
}

/// One resolved role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainRole<T> {
    pub domain: String,
    pub platform: String,
    pub role: Role,
    /// The (lower-cased) URL that revealed it.
    pub url: String,
    pub source: T,
}

impl<T> DomainRole<T> {
    /// Autopsy's `TSK_TEXT`: `"<role> (<platform>)"`.
    pub fn text(&self) -> String {
        format!("{} ({})", self.role.description(), self.platform)
    }
}

impl<T: Clone> RoleProcessor<T> {
    /// Feeds one artifact's URL and domain (lower-cased here).
    pub fn add(&mut self, url: &str, domain: &str, service_account: bool, source: &T) {
        let url = url.to_lowercase();
        let domain = domain.to_lowercase();
        for (platform, role) in roles_for_url(&url, &domain, service_account) {
            let key = (domain.clone(), platform);
            match self.roles.get_mut(&key) {
                Some(cur) if role > cur.0 => *cur = (role, url.clone(), source.clone()),
                Some(_) => {}
                None => {
                    self.order.push(key.clone());
                    self.roles.insert(key, (role, url.clone(), source.clone()));
                }
            }
        }
    }

    /// The resolved roles in first-seen order.
    pub fn finish(mut self) -> Vec<DomainRole<T>> {
        self.order
            .into_iter()
            .filter_map(|k| {
                let (role, url, source) = self.roles.remove(&k)?;
                Some(DomainRole {
                    domain: k.0,
                    platform: k.1,
                    role,
                    url,
                    source,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_rules() {
        assert_eq!(
            roles_for_url("http://f.com/admin/index.php", "f.com", false),
            vec![("myBB platform".into(), Role::Administrator)]
        );
        assert_eq!(
            roles_for_url("http://f.com/forum/ucp.php?mode=x", "f.com", false),
            vec![("phpBB platform".into(), Role::User)]
        );
        assert_eq!(
            roles_for_url("http://b.com/wp-admin/", "b.com", false),
            vec![("WordPress platform".into(), Role::User)]
        );
        assert_eq!(
            roles_for_url("http://b.com/wp-admin/options.php", "b.com", false),
            vec![("WordPress platform".into(), Role::Administrator)]
        );
        assert_eq!(
            roles_for_url("http://b.com/administrator/index.php", "b.com", false),
            vec![("Joomla platform".into(), Role::Administrator)]
        );
        assert!(roles_for_url("http://b.com/", "b.com", false).is_empty());
        assert_eq!(
            roles_for_url("http://b.com/login", "b.com", true),
            vec![("b.com".into(), Role::User)]
        );
    }

    #[test]
    fn highest_rank_wins() {
        let mut p = RoleProcessor::default();
        p.add("http://F.com/usercp.php", "F.com", false, &1);
        p.add("http://f.com/modcp.php", "f.com", false, &2);
        p.add("http://f.com/usercp.php", "f.com", false, &3);
        p.add("http://g.com/x", "g.com", true, &4);
        let r = p.finish();
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].role, Role::Moderator);
        assert_eq!(r[0].source, 2);
        assert_eq!(r[0].text(), "Moderator role (myBB platform)");
        assert_eq!(r[1].domain, "g.com");
        assert_eq!(r[1].platform, "g.com");
    }
}
