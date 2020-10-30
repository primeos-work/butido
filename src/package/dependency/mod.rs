use lazy_static::lazy_static;
use anyhow::anyhow;
use anyhow::Result;
use regex::Regex;

use crate::package::PackageName;
use crate::package::PackageVersionConstraint;

mod system;
pub use system::*;

mod system_runtime;
pub use system_runtime::*;

mod build;
pub use build::*;

mod runtime;
pub use runtime::*;

pub trait StringEqual {
    fn str_equal(&self, s: &str) -> bool;
}

pub trait ParseDependency {
    fn parse_into_name_and_version(self) -> Result<(PackageName, PackageVersionConstraint)>;
}

lazy_static! {
    pub(in crate::package::dependency)  static ref DEPENDENCY_PARSING_RE: Regex =
        Regex::new("^(?P<name>[[:alpha:]]([[[:alnum:]]-_])*) (?P<version>([\\*=><])?[[:alnum:]]([[[:alnum:]][[:punct:]]])*)$").unwrap();
}

/// Helper function for the actual implementation of the ParseDependency trait.
///
/// TODO: Reimplement using pom crate
pub(in crate::package::dependency) fn parse_package_dependency_string_into_name_and_version(s: &str)
    -> Result<(PackageName, PackageVersionConstraint)>
{
    let caps = crate::package::dependency::DEPENDENCY_PARSING_RE
        .captures(s)
        .ok_or_else(|| anyhow!("Could not parse into package name and package version constraint: '{}'", s))?;

    let name = caps.name("name")
        .ok_or_else(|| anyhow!("Could not parse name: '{}'", s))?;

    let vers = caps.name("version")
        .ok_or_else(|| anyhow!("Could not parse version: '{}'", s))?;

    let constraint = PackageVersionConstraint::parse(vers.as_str())?;

    Ok((PackageName::from(String::from(name.as_str())), constraint))
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::convert::TryInto;

    use crate::package::Package;
    use crate::package::PackageName;
    use crate::package::PackageVersion;
    use crate::package::PackageVersionConstraint;

    //
    // helper functions
    //

    fn name(s: &'static str) -> PackageName {
        PackageName::from(String::from(s))
    }

    fn exact(s: &'static str) -> PackageVersionConstraint {
        PackageVersionConstraint::Exact(PackageVersion::from(String::from(s)))
    }

    fn higher_as(s: &'static str) -> PackageVersionConstraint {
        PackageVersionConstraint::HigherAs(PackageVersion::from(String::from(s)))
    }

    //
    // tests
    //

    #[test]
    fn test_dependency_conversion_1() {
        let s = "vim =8.2";
        let d = Dependency::from(String::from(s));

        let (n, c) = d.try_into().unwrap();

        assert_eq!(n, name("vim"));
        assert_eq!(c, exact("8.2"));
    }

    #[test]
    fn test_dependency_conversion_2() {
        let s = "gtk15 >1b";
        let d = Dependency::from(String::from(s));

        let (n, c) = d.try_into().unwrap();

        assert_eq!(n, name("gtk15"));
        assert_eq!(c, higher_as("1b"));
    }
}
