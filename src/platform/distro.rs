//! 发行版识别:只为了给对一条安装命令。
//!
//! 单独成文件,是因为它和"怎么探测"无关:探测是 `probes`,这里只回答"这台机器
//! 上那个东西该怎么装"。
//!
//! ⚠ 认不出来时**不猜** —— 宁可什么都不给,也不要给一条跑不通的命令。

/// 发行版。
pub(super) struct Distro {
    pub(super) name: String,
    pub(super) family: Family,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Family {
    Arch,
    Debian,
    Fedora,
    Unknown,
}

/// 同一个东西在不同发行版里包名不同,所以按发行版各写一份。
#[derive(Debug, Clone, Copy)]
pub(super) struct Package {
    arch: &'static str,
    debian: &'static str,
    fedora: &'static str,
}

impl Package {
    pub(super) const fn same(name: &'static str) -> Self {
        Self {
            arch: name,
            debian: name,
            fedora: name,
        }
    }

    /// 包名按发行版各不相同的那种。
    pub(super) const fn per_distro(
        arch: &'static str,
        debian: &'static str,
        fedora: &'static str,
    ) -> Self {
        Self {
            arch,
            debian,
            fedora,
        }
    }
}

impl Distro {
    pub(super) fn detect() -> Self {
        if !cfg!(target_os = "linux") {
            return Self {
                name: String::new(),
                family: Family::Unknown,
            };
        }
        std::fs::read_to_string("/etc/os-release")
            .map(|text| Self::parse(&text))
            .unwrap_or(Self {
                name: String::new(),
                family: Family::Unknown,
            })
    }

    pub(super) fn parse(text: &str) -> Self {
        let (mut id, mut like, mut pretty) = (String::new(), String::new(), String::new());
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            // os-release 的值可以带引号,单双都有(实测 Manjaro 用单引号)。
            let value = value.trim().trim_matches(['"', '\'']);
            match key.trim() {
                "ID" => id = value.to_ascii_lowercase(),
                "ID_LIKE" => like = value.to_ascii_lowercase(),
                "PRETTY_NAME" => pretty = value.to_string(),
                _ => {}
            }
        }
        let haystack = format!("{id} {like}");
        let family = if haystack.contains("arch") {
            Family::Arch
        } else if haystack.contains("debian") || haystack.contains("ubuntu") {
            Family::Debian
        } else if haystack.contains("fedora")
            || haystack.contains("rhel")
            || haystack.contains("centos")
        {
            Family::Fedora
        } else {
            Family::Unknown
        };
        Self {
            name: if pretty.is_empty() { id } else { pretty },
            family,
        }
    }

    /// 这个包在这台机器上怎么装。空字符串 = 给不出命令(认不出发行版)。
    pub(super) fn install(&self, package: Package) -> String {
        let name = match self.family {
            Family::Arch => package.arch,
            Family::Debian => package.debian,
            Family::Fedora => package.fedora,
            Family::Unknown => return String::new(),
        };
        match self.family {
            Family::Arch => format!("sudo pacman -S {name}"),
            Family::Debian => format!("sudo apt install {name}"),
            Family::Fedora => format!("sudo dnf install {name}"),
            Family::Unknown => String::new(),
        }
    }
}
