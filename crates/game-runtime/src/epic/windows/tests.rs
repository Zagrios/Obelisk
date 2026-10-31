use super::*;

fn prerequisite(ids: &[&str], path: &str) -> epic_kit::client::Prerequisite {
    epic_kit::client::Prerequisite {
        ids: ids.iter().map(|id| (*id).into()).collect(),
        name: "Prerequisites".into(),
        path: path.into(),
        arguments: "/quiet /norestart".into(),
    }
}

#[test]
fn an_epic_prerequisite_runs_unless_it_leaves_the_installation() {
    let install = Path::new("/games/epic");
    let listed = |prerequisite: Option<&epic_kit::client::Prerequisite>| {
        prerequisites(install, prerequisite)
    };

    assert_eq!(
        listed(Some(&prerequisite(
            &["a", "b"],
            "Redist\\DirectX\\setup.exe"
        ))),
        [wine::Prerequisite {
            id: "epic:a,b".into(),
            programs: vec![(
                install.join("Redist/DirectX/setup.exe"),
                "/quiet /norestart".into()
            )],
        }]
    );
    assert_eq!(
        listed(Some(&prerequisite(&[], "Redist/setup.exe")))[0].id,
        "epic:Redist/setup.exe"
    );
    // Visual C++ and Unreal's prerequisites install as under Proton.
    assert_eq!(
        listed(Some(&prerequisite(
            &["ue"],
            "Engine\\Extras\\Redist\\en-us\\UEPrereqSetup_x64.exe"
        )))[0]
            .programs,
        [(
            install.join("Engine/Extras/Redist/en-us/UEPrereqSetup_x64.exe"),
            "/quiet /norestart".into()
        )]
    );
    for path in ["..\\outside.exe", "/Applications/setup.exe"] {
        assert!(
            listed(Some(&prerequisite(&["a"], path))).is_empty(),
            "{path}"
        );
    }
    assert!(listed(None).is_empty());
}
