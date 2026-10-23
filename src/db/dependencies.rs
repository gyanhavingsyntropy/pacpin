//! Satisfaction for loaded archives, whose Pkg wrapper has no find_satisfier API.
//! Repository selection still belongs exclusively to find_sync_package.
pub(crate) fn package_satisfies(package: &alpm::Pkg, dependency: &str) -> bool {
    let dependency = alpm::Depend::new(dependency);
    let matches = |version: Option<&alpm::Ver>| {
        use alpm::DepMod;
        if dependency.depmod() == DepMod::Any {
            return true;
        }
        let (Some(version), Some(required)) = (version, dependency.version()) else {
            return false;
        };
        let order = alpm::vercmp(version.as_str(), required.as_str());
        match dependency.depmod() {
            DepMod::Any => true,
            DepMod::Eq => order.is_eq(),
            DepMod::Ge => !order.is_lt(),
            DepMod::Le => !order.is_gt(),
            DepMod::Gt => order.is_gt(),
            DepMod::Lt => order.is_lt(),
        }
    };
    package.name() == dependency.name() && matches(Some(package.version()))
        || package
            .provides()
            .into_iter()
            .any(|provided| provided.name() == dependency.name() && matches(provided.version()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{AlpmFixture, FixturePackage};
    #[test]
    fn archive_satisfaction_matches_alpm_for_versions_and_providers() {
        let mut package = FixturePackage::new("real", "1:2.0-3");
        package.provides = &["virtual=5", "unversioned"];
        let fixture = AlpmFixture::new(&[package], &[]);
        let db = fixture.manager.handle().localdb();
        let package = db.pkg("real").unwrap();
        for dependency in [
            "real",
            "real>2.0",
            "real>=1:2.0-3",
            "real<1:2.0-3",
            "real=1:2.0-3",
            "real<=1:2.0-3",
            "real>1:2.0-3",
            "virtual",
            "virtual>=5",
            "virtual>5",
            "virtual<6",
            "unversioned",
            "unversioned>=1",
            "other",
        ] {
            assert_eq!(
                package_satisfies(package, dependency),
                db.pkgs().find_satisfier(dependency).is_some(),
                "{dependency}"
            );
        }
    }
}
