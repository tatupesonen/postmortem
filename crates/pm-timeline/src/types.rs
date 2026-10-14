//! Event type hierarchy (Autopsy's `TimelineEventType` tree).

/// Top-level event categories ("base types").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Category {
    FileSystem,
    WebActivity,
    Misc,
    Custom,
}

impl Category {
    pub const ALL: [Self; 4] = [
        Self::FileSystem,
        Self::WebActivity,
        Self::Misc,
        Self::Custom,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::FileSystem => "File System",
            Self::WebActivity => "Web Activity",
            Self::Misc => "Misc Types",
            Self::Custom => "Custom Types",
        }
    }

    fn index(self) -> usize {
        self as usize
    }

    /// Autopsy's colour: category hue = root hue + i·360/(n+1).
    pub fn color(self) -> [u8; 3] {
        hsb(category_hue(self), 0.9, 0.9)
    }
}

/// Leaf event types ("sub types").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EventType {
    FileModified,
    FileAccessed,
    FileCreated,
    FileChanged,
    WebHistory,
    WebDownloads,
    WebCookies,
    WebBookmarks,
    WebSearches,
    WebFormAutofill,
    Messages,
    Calls,
    Email,
    GpsRoutes,
    GpsTrackpoint,
    RecentDocuments,
    InstalledPrograms,
    Exif,
    DevicesAttached,
    LogEntry,
    Registry,
    ProgramExecution,
    UserCreated,
    OtherArtifact,
}

impl EventType {
    pub const ALL: [Self; 24] = [
        Self::FileModified,
        Self::FileAccessed,
        Self::FileCreated,
        Self::FileChanged,
        Self::WebHistory,
        Self::WebDownloads,
        Self::WebCookies,
        Self::WebBookmarks,
        Self::WebSearches,
        Self::WebFormAutofill,
        Self::Messages,
        Self::Calls,
        Self::Email,
        Self::GpsRoutes,
        Self::GpsTrackpoint,
        Self::RecentDocuments,
        Self::InstalledPrograms,
        Self::Exif,
        Self::DevicesAttached,
        Self::LogEntry,
        Self::Registry,
        Self::ProgramExecution,
        Self::UserCreated,
        Self::OtherArtifact,
    ];
    pub const COUNT: usize = Self::ALL.len();

    pub fn index(self) -> usize {
        self as usize
    }

    pub fn category(self) -> Category {
        use EventType::{
            FileAccessed, FileChanged, FileCreated, FileModified, OtherArtifact, UserCreated,
            WebBookmarks, WebCookies, WebDownloads, WebFormAutofill, WebHistory, WebSearches,
        };
        match self {
            FileModified | FileAccessed | FileCreated | FileChanged => Category::FileSystem,
            WebHistory | WebDownloads | WebCookies | WebBookmarks | WebSearches
            | WebFormAutofill => Category::WebActivity,
            UserCreated | OtherArtifact => Category::Custom,
            _ => Category::Misc,
        }
    }

    pub fn name(self) -> &'static str {
        use EventType::{
            Calls, DevicesAttached, Email, Exif, FileAccessed, FileChanged, FileCreated,
            FileModified, GpsRoutes, GpsTrackpoint, InstalledPrograms, LogEntry, Messages,
            OtherArtifact, ProgramExecution, RecentDocuments, Registry, UserCreated, WebBookmarks,
            WebCookies, WebDownloads, WebFormAutofill, WebHistory, WebSearches,
        };
        match self {
            FileModified => "File Modified",
            FileAccessed => "File Accessed",
            FileCreated => "File Created",
            FileChanged => "File Changed",
            WebHistory => "Web History",
            WebDownloads => "Web Downloads",
            WebCookies => "Web Cookies",
            WebBookmarks => "Web Bookmarks",
            WebSearches => "Web Searches",
            WebFormAutofill => "Web Form Autofill",
            Messages => "Messages",
            Calls => "Calls",
            Email => "Email",
            GpsRoutes => "GPS Routes",
            GpsTrackpoint => "Location History",
            RecentDocuments => "Recent Documents",
            InstalledPrograms => "Installed Programs",
            Exif => "Exif",
            DevicesAttached => "Devices Attached",
            LogEntry => "Log Entry",
            Registry => "Registry",
            ProgramExecution => "Program Execution",
            UserCreated => "User Created",
            OtherArtifact => "Other Artifact",
        }
    }

    /// MACB letter for file-system types (`M`, `A`, `B`orn, `C`hanged).
    pub fn macb_letter(self) -> Option<char> {
        match self {
            Self::FileModified => Some('M'),
            Self::FileAccessed => Some('A'),
            Self::FileCreated => Some('B'),
            Self::FileChanged => Some('C'),
            _ => None,
        }
    }

    /// Sub-types of a category, in display order.
    pub fn of(category: Category) -> impl Iterator<Item = Self> {
        Self::ALL
            .into_iter()
            .filter(move |t| t.category() == category)
    }

    /// Sub-type colour. Autopsy uses hue = category hue + j·(360/nCat)/
    /// (maxChildren+1), which makes siblings nearly indistinguishable when
    /// stacked; we spread siblings over most of the category's hue band and
    /// alternate brightness so adjacent segments always contrast.
    pub fn color(self) -> [u8; 3] {
        let cat = self.category();
        let j = Self::of(cat).position(|t| t == self).unwrap_or(0);
        let n = Self::of(cat).count().max(1) as f32;
        let band = 360.0 / (Category::ALL.len() as f32 + 1.0);
        let hue = category_hue(cat) - band * 0.45 + band * 0.9 * (j as f32 + 0.5) / n;
        let brightness = if j % 2 == 0 { 0.95 } else { 0.7 };
        hsb(hue, 0.85, brightness)
    }
}

const ROOT_HUE: f32 = 359.0;

fn category_hue(c: Category) -> f32 {
    ROOT_HUE + (c.index() as f32 + 1.0) * 360.0 / (Category::ALL.len() as f32 + 1.0)
}

/// HSB/HSV (hue in degrees) to RGB.
pub fn hsb(hue: f32, saturation: f32, brightness: f32) -> [u8; 3] {
    let sector = hue.rem_euclid(360.0) / 60.0;
    let chroma = brightness * saturation;
    let second = chroma * (1.0 - (sector % 2.0 - 1.0).abs());
    let (red, green, blue) = match sector as u32 {
        0 => (chroma, second, 0.0),
        1 => (second, chroma, 0.0),
        2 => (0.0, chroma, second),
        3 => (0.0, second, chroma),
        4 => (second, 0.0, chroma),
        _ => (chroma, 0.0, second),
    };
    let offset = brightness - chroma;
    let to = |v: f32| ((v + offset) * 255.0).round().clamp(0.0, 255.0) as u8;
    [to(red), to(green), to(blue)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hierarchy() {
        assert_eq!(EventType::of(Category::FileSystem).count(), 4);
        assert!(
            EventType::ALL
                .iter()
                .all(|t| EventType::of(t.category()).any(|x| x == *t))
        );
        assert_eq!(
            EventType::ALL
                .iter()
                .enumerate()
                .filter(|(i, t)| t.index() != *i)
                .count(),
            0
        );
    }

    #[test]
    fn colours_are_distinct_and_hsb_correct() {
        assert_eq!(hsb(0.0, 1.0, 1.0), [255, 0, 0]);
        assert_eq!(hsb(120.0, 1.0, 1.0), [0, 255, 0]);
        assert_eq!(hsb(240.0, 1.0, 1.0), [0, 0, 255]);
        let mut cats: Vec<_> = Category::ALL.iter().map(|c| c.color()).collect();
        cats.dedup();
        assert_eq!(cats.len(), 4);
        let fs: std::collections::HashSet<_> = EventType::of(Category::FileSystem)
            .map(EventType::color)
            .collect();
        assert_eq!(fs.len(), 4);
    }

    #[test]
    fn macb() {
        let s: String = EventType::of(Category::FileSystem)
            .filter_map(EventType::macb_letter)
            .collect();
        assert_eq!(s, "MABC");
    }
}
