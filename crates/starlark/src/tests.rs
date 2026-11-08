use std::path::Path;

use distro_eval::{
    ConfigFs, ConfigKind, DirFs, Evaluation, Input, MemFs, Module, OptionPath, OptionSpec, Output,
    Schema, Sources, Type, describe_outputs, evaluate, explain,
};

use super::load;

const MODULES: [&str; 8] = [
    "firewall",
    "fprintd",
    "nginx",
    "pam",
    "pam-base",
    "syncthing",
    "system",
    "users",
];

fn repo() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}

fn base_schema() -> Schema {
    let mut schema = Schema::default();
    schema
        .declare("system.hostname", OptionSpec::new(Type::Str))
        .unwrap();
    schema
}

/// Evaluates a fixture config with the named built-in modules.
fn evaluate_fixture(config: &str, names: &[&str]) -> Evaluation {
    // Relative, so locations in snapshots don't depend on where the repository is.
    let config = DirFs::new(format!("../../fixtures/configs/{config}"));
    evaluate_with(&config, names).unwrap_or_else(|e| panic!("{e}"))
}

/// Evaluates a config with the named built-in modules, with errors rendered.
fn evaluate_with(config: &dyn ConfigFs, names: &[&str]) -> Result<Evaluation, String> {
    let mut sources = Sources::default();
    let modules: Vec<Box<dyn Module>> = names
        .iter()
        .map(|name| {
            let path = format!("modules/{name}.star");
            let text = std::fs::read_to_string(repo().join(&path)).unwrap();
            load(&mut sources, &path, text, &repo().join("modules"))
                .unwrap_or_else(|e| panic!("{}", distro_eval::render(&[e], &sources)))
        })
        .collect();
    evaluate(Input {
        sources,
        config,
        kind: ConfigKind::System,
        user: None,
        role: None,
        schema: Schema::default(),
        modules: &modules,
    })
    .map_err(|e| e.to_string())
}

fn run(source: &str) -> Result<Evaluation, String> {
    let mut sources = Sources::default();
    let module = load(
        &mut sources,
        "test.star",
        source.to_owned(),
        &repo().join("modules"),
    )
    .map_err(|e| distro_eval::render(&[e], &sources))?;
    evaluate(Input {
        sources,
        config: &MemFs::new([("system.toml", "[system]\nhostname = \"box\"\n")]),
        kind: ConfigKind::System,
        user: None,
        role: None,
        schema: base_schema(),
        modules: &[module],
    })
    .map_err(|e| e.to_string())
}

#[test]
fn the_built_in_modules_render_the_fixture() {
    let evaluation = evaluate_fixture("s4", &MODULES);

    insta::assert_snapshot!("s4_outputs", describe_outputs(&evaluation));
    let why = |path: &str| explain(&evaluation, Some(&OptionPath::parse(path).unwrap())).unwrap();
    insta::assert_snapshot!(why("security.pam.services.sudo.auth"), @r#"
    security.pam.services.sudo.auth = [{ args = ["conffile=/etc/pam/environment", "readenv=0"], control = "required", module = "pam_env.so", name = "env" }, { control = "sufficient", module = "pam_fprintd.so", name = "fprintd" }, { args = ["likeauth", "try_first_pass"], control = "sufficient", module = "pam_unix.so", name = "unix" }]
      set by module `fprintd` at modules/fprintd.star:14:9: [{ after = "env", before = "unix", control = "sufficient", module = "pam_fprintd.so", name = "fprintd" }]
      set by module `pam-base` at modules/pam-base.star:7:9: [{ args = ["conffile=/etc/pam/environment", "readenv=0"], control = "required", module = "pam_env.so", name = "env" }, { after = "env", args = ["likeauth", "try_first_pass"], control = "sufficient", module = "pam_unix.so", name = "unix" }, { after = "unix", control = "required", module = "pam_deny.so", name = "deny" }]
      overrides [], default
      removes ["deny"], set at ../../fixtures/configs/s4/system.toml:9:8
    "#);
}

#[test]
fn settings_point_at_the_exact_call() {
    let evaluation = evaluate_fixture("s4", &MODULES);

    insta::assert_snapshot!(
        explain(&evaluation, Some(&OptionPath::parse("networking.firewall.allowed_tcp_ports").unwrap())).unwrap(),
        @"
    networking.firewall.allowed_tcp_ports = [80, 22000]
      set by module `nginx` at modules/nginx.star:60:9: [80]
      set by module `syncthing` at modules/syncthing.star:209:9: [22000]
      overrides [], default
    "
    );
}

#[test]
fn errors_point_at_the_expression() {
    insta::assert_snapshot!(run(r#"
def render(ctx):
    ctx.read("system.hostnam")

module(name = "typo", options = {}, writes = [], render = render)
"#).unwrap_err(), @r#"
    error: unknown option `system.hostnam`; did you mean `system.hostname`?
     --> test.star:3:5
      |
    3 |     ctx.read("system.hostnam")
      |     ^^^^^^^^^^^^^^^^^^^^^^^^^^ here
    "#);
}

#[test]
fn values_read_are_copies() {
    let evaluation = run(r#"
def render(ctx):
    system = ctx.read("system")
    system["hostname"] = "other"
    ctx.file("/hostname", ctx.read("system.hostname"))

module(name = "mutate", options = {}, writes = [], render = render)
"#)
    .unwrap();

    assert_eq!(
        evaluation.modules()[0].output.files["/hostname"].text,
        "box"
    );
}

#[test]
fn a_module_that_runs_too_long_is_stopped() {
    let message = run(r#"
def render(ctx):
    for _ in range(1000000000):
        pass

module(name = "spin", options = {}, writes = [], render = render)
"#)
    .unwrap_err();

    assert!(message.contains("cancel"), "{message}");
}

#[test]
fn a_module_must_declare_itself() {
    let message = run("x = 1\n").unwrap_err();

    assert!(message.contains("never calls `module(...)`"), "{message}");
}

#[test]
fn types_and_options_are_checked() {
    let message = run(r#"
module(name = "x", options = {"a": types.list(option(types.str))}, writes = [], render = None)
"#)
    .unwrap_err();

    assert!(message.contains("expected a type"), "{message}");
}

fn output_of<'a>(evaluation: &'a Evaluation, module: &str) -> &'a Output {
    &evaluation
        .modules()
        .iter()
        .find(|rendered| rendered.name == module)
        .unwrap_or_else(|| panic!("no module `{module}`"))
        .output
}

/// The files, units and triggers one module renders, as text.
fn rendered_by(evaluation: &Evaluation, module: &str) -> String {
    let output = output_of(evaluation, module);
    let mut out = String::new();
    for (path, file) in &output.files {
        out.push_str(&format!("{path}\n{}\n", file.text));
    }
    for (name, unit) in &output.units {
        out.push_str(&format!("{name}\n{}\n", unit.text()));
    }
    for ((unit, action), paths) in &output.triggers {
        out.push_str(&format!("{} {unit} on {paths:?}\n", action.as_str()));
    }
    out
}

const NETWORK_MODULES: [&str; 5] = ["base", "firewall", "network", "system", "tailscale"];

#[test]
fn networks_and_dns_render_for_networkd_and_resolved() {
    let evaluation = evaluate_fixture("network", &NETWORK_MODULES);

    insta::assert_snapshot!(rendered_by(&evaluation, "network"), @r#"
    /etc/systemd/network/10-lan.network
    [Match]
    Name=enp*
    Name=wlan*
    Name=wlp*

    [Network]
    DHCP=yes
    IPv6AcceptRA=yes
    IPv6PrivacyExtensions=yes
    IgnoreCarrierLoss=3s
    LLDP=yes

    /etc/systemd/resolved.conf.d/distro.conf
    [Resolve]
    Cache=no-negative
    DNSSEC=no
    FallbackDNS=9.9.9.9 149.112.112.112

    reload systemd-networkd.service on {"/etc/systemd/network/10-lan.network"}
    restart systemd-resolved.service on {"/etc/systemd/resolved.conf.d/distro.conf"}
    "#);
}

#[test]
fn tailscale_runs_its_agent_and_networkd_leaves_its_interface_alone() {
    let evaluation = evaluate_fixture("network", &NETWORK_MODULES);

    insta::assert_snapshot!(rendered_by(&evaluation, "tailscale"), @r#"
    /etc/systemd/network/50-tailscale.network
    [Match]
    Name=tailscale0

    [Link]
    ActivationPolicy=manual
    Unmanaged=yes

    tailscaled.service
    [Unit]
    After=network-pre.target
    After=network-online.target
    After=systemd-resolved.service
    Description=Tailscale node agent
    Wants=network-pre.target
    Wants=network-online.target

    [Service]
    CacheDirectory=tailscale
    CacheDirectoryMode=0750
    ExecStart=tailscaled --state=/var/lib/tailscale/tailscaled.state --socket=/run/tailscale/tailscaled.sock --port=41641 --tun tailscale0
    ExecStopPost=tailscaled --cleanup
    Restart=on-failure
    RuntimeDirectory=tailscale
    RuntimeDirectoryMode=0755
    StateDirectory=tailscale
    StateDirectoryMode=0700
    Type=notify

    [Install]
    WantedBy=multi-user.target

    reload systemd-networkd.service on {"/etc/systemd/network/50-tailscale.network"}
    "#);
    let firewall = rendered_by(&evaluation, "firewall");
    assert!(
        firewall.contains("iifname \"tailscale0\" accept"),
        "{firewall}"
    );
}

#[test]
fn ollama_runs_sandboxed_with_the_package_for_its_gpus() {
    let evaluation = evaluate_fixture("ollama", &["base", "firewall", "ollama", "system"]);

    insta::assert_snapshot!(rendered_by(&evaluation, "ollama"), @r#"
    ollama.service
    [Unit]
    After=network.target
    Description=Server for local large language models

    [Service]
    CapabilityBoundingSet=
    DeviceAllow=char-nvidiactl
    DeviceAllow=char-nvidia-caps
    DeviceAllow=char-nvidia-frontend
    DeviceAllow=char-nvidia-uvm
    DeviceAllow=char-drm
    DeviceAllow=char-fb
    DeviceAllow=char-kfd
    DevicePolicy=closed
    DynamicUser=yes
    Environment="HOME=/var/lib/ollama"
    Environment="OLLAMA_MODELS=/var/lib/ollama/models"
    Environment="OLLAMA_HOST=0.0.0.0:11434"
    Environment="HSA_OVERRIDE_GFX_VERSION=11.0.0"
    Environment="OLLAMA_IGPU_ENABLE=1"
    ExecStart=ollama serve
    LockPersonality=yes
    MemoryDenyWriteExecute=yes
    NoNewPrivileges=yes
    PrivateDevices=no
    PrivateTmp=yes
    PrivateUsers=yes
    ProcSubset=all
    ProtectClock=yes
    ProtectControlGroups=yes
    ProtectHome=yes
    ProtectHostname=yes
    ProtectKernelLogs=yes
    ProtectKernelModules=yes
    ProtectKernelTunables=yes
    ProtectProc=invisible
    ProtectSystem=strict
    RemoveIPC=yes
    RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX
    RestrictNamespaces=yes
    RestrictRealtime=yes
    RestrictSUIDSGID=yes
    StateDirectory=ollama
    SupplementaryGroups=render
    SystemCallArchitectures=native
    SystemCallFilter=@system-service @resources
    SystemCallFilter=~@privileged
    Type=exec
    UMask=0077
    WorkingDirectory=/var/lib/ollama

    [Install]
    WantedBy=multi-user.target
    "#);
    assert_eq!(
        output_of(&evaluation, "ollama").packages,
        ["ollama-rocm".to_owned()].into()
    );
    assert!(rendered_by(&evaluation, "firewall").contains("tcp dport 11434 accept"));
}

const SECRET_MODULES: [&str; 5] = ["files", "secrets", "syncthing", "system", "users"];

#[test]
fn secrets_are_declared_with_their_encrypted_files_and_paths_take_them() {
    let evaluation = evaluate_fixture("secrets", &SECRET_MODULES);

    insta::assert_debug_snapshot!(output_of(&evaluation, "secrets").secrets);
    let ciphertext =
        std::fs::read(repo().join("fixtures/configs/secrets/secrets/api-token.age")).unwrap();
    let hash = blake3::hash(&ciphertext).to_hex().to_string();
    let files = evaluation.secret_files();
    assert_eq!(files.keys().collect::<Vec<_>>(), ["api-token", "sync-key"]);
    for file in files.values() {
        assert!(
            file.source.ends_with("secrets/api-token.age"),
            "{}",
            file.source
        );
        assert_eq!(file.hash, hash);
    }
    let syncthing = rendered_by(&evaluation, "syncthing");
    assert!(
        syncthing.contains("/run/secrets/sync-key /var/lib/syncthing/.config/syncthing/key.pem"),
        "{syncthing}"
    );
}

#[test]
fn files_with_secrets_are_filled_in_on_the_machine() {
    let evaluation = evaluate_fixture("secrets", &SECRET_MODULES);
    let templates = &output_of(&evaluation, "files").templates;

    insta::assert_debug_snapshot!(templates, @r#"
    {
        "/etc/app/env": Template {
            parts: [
                Text(
                    "TOKEN=",
                ),
                Secret(
                    "api-token",
                ),
                Text(
                    "\nMODE=prod\n",
                ),
            ],
            owner: "syncthing",
            group: None,
            mode: 256,
        },
    }
    "#);
}

#[test]
fn formats_and_plus_keep_secrets_for_the_machine_to_fill_in() {
    let module = r#"
def render(ctx):
    token = ctx.read("services.app.token")
    ctx.file("/etc/app.toml", formats.toml({"token": token, "port": 80}), owner = "app")
    ctx.file("/etc/app.env", "TOKEN=" + token + "\n", 0o440, group = "app")

module(
    name = "app",
    options = {"services.app.token": option(types.str, runtime = True)},
    writes = [],
    render = render,
)
"#;
    let mut sources = Sources::default();
    let secrets = std::fs::read_to_string(repo().join("modules/secrets.star")).unwrap();
    let modules = vec![
        load(
            &mut sources,
            "app.star",
            module.to_owned(),
            &repo().join("modules"),
        )
        .unwrap(),
        load(
            &mut sources,
            "modules/secrets.star",
            secrets,
            &repo().join("modules"),
        )
        .unwrap(),
    ];
    let config = MemFs::new([
        (
            "system.toml",
            "[system]\nhostname = \"box\"\n\n[secrets.tok]\n\n\
             [services.app]\ntoken = \"${secret.tok}\"\n",
        ),
        ("secrets/tok.age", ""),
    ]);
    let evaluation = evaluate(Input {
        sources,
        config: &config,
        kind: ConfigKind::System,
        user: None,
        role: None,
        schema: base_schema(),
        modules: &modules,
    })
    .unwrap_or_else(|e| panic!("{e}"));

    insta::assert_debug_snapshot!(output_of(&evaluation, "app").templates, @r#"
    {
        "/etc/app.env": Template {
            parts: [
                Text(
                    "TOKEN=",
                ),
                Secret(
                    "tok",
                ),
                Text(
                    "\n",
                ),
            ],
            owner: "root",
            group: Some(
                "app",
            ),
            mode: 288,
        },
        "/etc/app.toml": Template {
            parts: [
                Text(
                    "port = 80\ntoken = \"",
                ),
                QuotedSecret(
                    "tok",
                ),
                Text(
                    "\"\n",
                ),
            ],
            owner: "app",
            group: None,
            mode: 256,
        },
    }
    "#);
}

#[test]
fn only_files_with_secrets_have_an_owner() {
    let error = run(r#"
def render(ctx):
    ctx.file("/etc/plain", "x\n", owner = "app")

module(name = "plain", options = {}, writes = [], render = render)
"#)
    .unwrap_err();

    assert!(
        error.contains("only a file with secrets in it has an owner"),
        "{error}"
    );
}

#[test]
fn a_secret_must_be_declared_and_its_file_readable() {
    let config = |reference: &str| {
        let system = format!(
            "[system]\nhostname = \"box\"\n\n[secrets.api-token]\n\n[services.syncthing.main]\n\
             key = {{ secret = \"{reference}\" }}\n"
        );
        MemFs::new([("system.toml", system.as_str())])
    };

    insta::assert_snapshot!(evaluate_with(&config("api-tokn"), &SECRET_MODULES).unwrap_err(), @r#"
    error: there is no secret `api-tokn`
     --> system.toml:7:7
      |
    7 | key = { secret = "api-tokn" }
      |       ^^^^^^^^^^^^^^^^^^^^^^^ `services.syncthing.main.key` refers to it
      = help: did you mean `api-token`?
    "#);
    insta::assert_snapshot!(evaluate_with(&config("api-token"), &SECRET_MODULES).unwrap_err(), @"
    error: cannot read secret `api-token` from secrets/api-token.age: entity not found
     = help: encrypt it with age to the machine's key, or set its `file`
    ");
}

/// The files and packages one module puts in users' homes, as text.
fn homes_of(evaluation: &Evaluation, module: &str) -> String {
    let mut out = String::new();
    for (user, home) in &output_of(evaluation, module).homes {
        for (path, file) in &home.files {
            out.push_str(&format!("{user} {path}\n{}\n", file.text));
        }
        for (name, unit) in &home.units {
            out.push_str(&format!("{user} unit {name}\n{}\n", unit.text()));
        }
        for (path, template) in &home.templates {
            let owner = format!(
                "{}:{:?} {:o}",
                template.owner, template.group, template.mode
            );
            out.push_str(&format!(
                "{user} {path} with secrets for {owner}\n{:?}\n",
                template.parts
            ));
        }
        for package in &home.packages {
            out.push_str(&format!("{user} package {package}\n"));
        }
    }
    out
}

const HOME_MODULES: [&str; 8] = [
    "files",
    "git",
    "jujutsu",
    "secrets",
    "session",
    "system",
    "user-services",
    "users",
];

#[test]
fn users_run_their_own_services() {
    let evaluation = evaluate_fixture("home", &HOME_MODULES);

    insta::assert_snapshot!(homes_of(&evaluation, "user-services"), @r#"
    john unit manual.service
    [Unit]
    Description=manual

    [Service]
    ExecStart=sleep infinity
    Restart=on-failure

    john unit notes-sync.service
    [Unit]
    After=network-online.target
    Description=Sync notes

    [Service]
    Environment="NOTES_DIR=/srv/my notes"
    ExecStart=notes sync --watch
    Nice=10
    Restart=on-failure

    [Install]
    WantedBy=default.target
    "#);
    let users = &output_of(&evaluation, "users").base["users"];
    assert!(users.to_string().contains("linger = true"), "{users}");
}

#[test]
fn environment_reaches_systemd_and_login_shells() {
    let evaluation = evaluate_fixture("home", &HOME_MODULES);

    insta::assert_snapshot!(homes_of(&evaluation, "session"), @r#"
    john ~/.config/distro/session.sh
    export EDITOR="hx"
    export GREETING="it's \"quoted\""
    export PATH="$HOME/bin:${PATH}"

    john ~/.config/environment.d/60-distro.conf
    EDITOR=hx
    GREETING=it's "quoted"
    PATH=$HOME/bin:${PATH}
    "#);
    assert_eq!(
        output_of(&evaluation, "session").next_login,
        [
            "~john/.config/distro/session.sh".to_owned(),
            "~john/.config/environment.d/60-distro.conf".to_owned(),
        ]
        .into(),
    );
}

#[test]
fn environment_variables_cannot_run_commands() {
    let config = MemFs::new([
        ("system.toml", "[system]\nhostname = \"box\"\n"),
        ("users/john.toml", "environment.X = \"$(id)\"\n"),
    ]);

    let error = evaluate_with(&config, &HOME_MODULES).unwrap_err();
    assert!(error.contains("users.john.environment.X"), "{error}");
}

#[test]
fn users_configure_git_and_files_in_their_homes() {
    let evaluation = evaluate_fixture("home", &HOME_MODULES);

    insta::assert_snapshot!(homes_of(&evaluation, "git"), @r#"
    john ~/.config/git/attributes
    * merge=mergiraf

    john ~/.config/git/config
    [commit]
    	gpgSign = true
    [core]
    	pager = delta
    [filter "lfs"]
    	clean = git-lfs clean -- %f
    	process = git-lfs filter-process
    	required = true
    	smudge = git-lfs smudge -- %f
    [gpg]
    	format = ssh
    [init]
    	defaultBranch = main
    [interactive]
    	diffFilter = delta --color-only
    [merge "mergiraf"]
    	driver = mergiraf merge --git %O %A %B -s %S
    	name = mergiraf
    [tag]
    	gpgSign = true
    [url "https://github.com/"]
    	insteadOf = gh:
    [user]
    	email = john@example.com
    	name = John
    	signingKey = /run/secrets/git-signing
    [includeIf "gitdir:~/work/"]
    	path = ~/.config/git/include-1

    john ~/.config/git/ignore
    *~
    .direnv*

    john ~/.config/git/include-1
    [user]
    	email = john@work.example

    john package delta
    john package git
    john package git-lfs
    "#);
    insta::assert_snapshot!(homes_of(&evaluation, "jujutsu"), @r#"
    john ~/.config/jj/config.toml
    [signing]
    backend = "ssh"
    behavior = "own"
    key = "/run/secrets/git-signing"

    [ui]
    diff-formatter = ["difft", "--color=always", "$left", "$right"]
    editor = "hx"

    [user]
    email = "john@example.com"
    name = "John"

    john package jujutsu
    "#);
    insta::assert_snapshot!(homes_of(&evaluation, "files"), @r#"
    john ~/.config/helix/config.toml
    theme = "onedark"

    john ~/.netrc with secrets for john:None 400
    [Text("machine example.com password "), Secret("git-signing"), Text("\n")]
    "#);
}

#[test]
fn home_files_start_with_a_tilde() {
    let config = MemFs::new([
        ("system.toml", "[system]\nhostname = \"box\"\n"),
        ("users/john.toml", "[files.\".bashrc\"]\ntext = \"\"\n"),
    ]);

    insta::assert_snapshot!(evaluate_with(&config, &HOME_MODULES).unwrap_err(), @r#"
    error: `.bashrc` doesn't start with `~/`
     --> users/john.toml:2:8
      |
    2 | text = ""
      |        ^^ `users.john.files.".bashrc"` is set here
      = reported by modules/files.star
    "#);
}

/// Loads `module` with `files` in a temporary modules directory it can `load` from.
fn load_from(files: &[(&str, &str)], module: &str) -> Result<Evaluation, String> {
    let dir = tempfile::tempdir().unwrap();
    for (path, text) in files {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    let mut sources = Sources::default();
    let module = load(&mut sources, "app.star", module.to_owned(), dir.path())
        .map_err(|e| distro_eval::render(&[e], &sources))?;
    evaluate(Input {
        sources,
        config: &MemFs::new([("system.toml", "[system]\nhostname = \"box\"\n")]),
        kind: ConfigKind::System,
        user: None,
        role: None,
        schema: base_schema(),
        modules: &[module],
    })
    .map_err(|e| e.to_string())
}

const LOADS_MERGE: &str = r#"
load("lib/merge.star", "merge")

def render(ctx):
    ctx.file("/etc/merged", str(merge({"a": {"b": 1}}, {"a": {"c": 2}})) + "\n")

module(name = "app", options = {}, writes = [], render = render)
"#;

#[test]
fn modules_load_helpers_from_libraries() {
    let files = [
        (
            "lib/merge.star",
            "load(\"lib/deep.star\", \"deep\")\n\ndef merge(a, b):\n    return deep(a, b)\n",
        ),
        (
            "lib/deep.star",
            "def deep(a, b):\n    out = dict(a)\n    for k, v in b.items():\n        \
             out[k] = deep(out[k], v) if type(v) == \"dict\" and k in out else v\n    return out\n",
        ),
    ];

    let evaluation = load_from(&files, LOADS_MERGE).unwrap();

    let file = &output_of(&evaluation, "app").files["/etc/merged"];
    assert_eq!(file.text, "{\"a\": {\"b\": 1, \"c\": 2}}\n");
}

#[test]
fn libraries_hold_helpers_only_and_stay_in_the_modules_directory() {
    let calls_module = "module(name = \"lib\", options = {}, writes = [], render = None)\n";
    let error = load_from(&[("lib/merge.star", calls_module)], LOADS_MERGE).unwrap_err();
    assert!(
        error.contains("only a module calls `module(...)`"),
        "{error}"
    );

    let error = load_from(&[], LOADS_MERGE).unwrap_err();
    assert!(error.contains("cannot load `lib/merge.star`"), "{error}");

    let outside = LOADS_MERGE.replace("lib/merge.star", "../merge.star");
    let error = load_from(&[], &outside).unwrap_err();
    assert!(error.contains("`../merge.star` isn't a library"), "{error}");

    let cycle = [("lib/merge.star", "load(\"lib/merge.star\", \"merge\")\n")];
    let error = load_from(&cycle, LOADS_MERGE).unwrap_err();
    assert!(error.contains("loads itself"), "{error}");
}

#[test]
fn an_error_about_an_option_the_configuration_doesnt_set_points_at_the_module() {
    let error = run(r#"
def render(ctx):
    ctx.error("system.timezone", "pick a timezone")

module(name = "picky", options = {}, writes = [], render = render)
"#)
    .unwrap_err();

    insta::assert_snapshot!(error, @r#"
    error: pick a timezone
     --> test.star:3:5
      |
    3 |     ctx.error("system.timezone", "pick a timezone")
      |     ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ here
    "#);
}

#[test]
fn a_module_says_which_configurations_it_belongs_to() {
    let module = |configurations: &str| {
        format!(
            "module(name = \"m\", options = {{}}, writes = [], render = lambda ctx: None, \
             configurations = {configurations})"
        )
    };
    let load_one = |source: String| {
        let mut sources = Sources::default();
        load(&mut sources, "m.star", source, &repo().join("modules"))
            .map(|module| module.configurations().to_vec())
            .map_err(|e| distro_eval::render(&[e], &sources))
    };

    assert_eq!(
        load_one(module("[\"system\", \"project\"]")),
        Ok(vec![ConfigKind::System, ConfigKind::Project])
    );
    let error = load_one(module("[\"laptop\"]")).unwrap_err();
    assert!(error.contains("not `laptop`"), "{error}");
}

#[test]
fn services_wait_for_what_they_need_and_for_being_ready() {
    let config = MemFs::new([
        ("system.toml", ""),
        (
            "users/john.toml",
            "[services.db]\ncommand = [\"postgres\", \"-D\", \"/srv/my db\"]\nready = \"tcp:5432\"\n\
             restart = \"always\"\n\n\
             [services.migrate]\ncommand = [\"migrate\", \"--to\", \"$LATEST\"]\nready = \"exit\"\n\
             needs = [\"db\"]\nautostart = false\n",
        ),
    ]);
    let evaluation = evaluate_with(&config, &["users", "user-services"]).unwrap();

    insta::assert_snapshot!(homes_of(&evaluation, "user-services"), @r#"
    john unit db.service
    [Unit]
    Description=db

    [Service]
    ExecStart=postgres -D "/srv/my db"
    ExecStartPost=bash -c 'until (exec 3<>/dev/tcp/127.0.0.1/5432) 2>/dev/null; do sleep 0.2; done'
    Restart=always

    [Install]
    WantedBy=default.target

    john unit migrate.service
    [Unit]
    After=db.service
    Description=migrate
    Requires=db.service

    [Service]
    ExecStart=migrate --to "$$LATEST"
    RemainAfterExit=yes
    Type=oneshot
    "#);
}

#[test]
fn services_say_what_they_need_and_how_they_are_ready() {
    let config = MemFs::new([
        ("system.toml", ""),
        (
            "users/john.toml",
            "[services.app]\ncommand = [\"app\"]\nready = \"tcp:http\"\nneeds = [\"dbb\"]\n",
        ),
    ]);
    let error = evaluate_with(&config, &["users", "user-services"]).unwrap_err();
    insta::assert_snapshot!(error, @r#"
    error: `ready` is `started`, `exit` or `tcp:PORT`, not `tcp:http`
     --> users/john.toml:3:9
      |
    3 | ready = "tcp:http"
      |         ^^^^^^^^^^ `users.john.services.app.ready` is set here
      = reported by modules/user-services.star
    "#);
}

/// The storage layout the `disks` module renders for `system`, as JSON.
fn storage_of(system: &str) -> Result<String, String> {
    let config = MemFs::new([("system.toml", system)]);
    let evaluation = evaluate_with(&config, &["disks"])?;
    let storage = &output_of(&evaluation, "disks").base["storage"];
    Ok(distro_eval::formats::json(storage).unwrap())
}

#[test]
fn disks_are_laid_out_and_mounted_by_name() {
    let raid = storage_of(
        "[disks.nvme]\ndevices = { count = 2, kind = \"nvme\", at_least = \"500G\" }\n\
         partitions.esp = { size = \"1G\", format = \"vfat\", mount = \"/boot\", on = \"first\" }\n\
         partitions.system = { size = \"rest\", encrypt = true }\n\n\
         [filesystems.root]\nraid = \"raid1\"\non = [\"nvme.system\"]\n\
         subvolumes.\"@root\" = { mount = \"/\" }\n\
         subvolumes.\"@nix\" = { mount = \"/nix\", options = [\"noatime\"] }\n\
         subvolumes.\"@persist\" = {}\n",
    )
    .unwrap();
    insta::assert_snapshot!(raid, @r#"
    {
      "disks": {
        "nvme": {
          "at_least": "500G",
          "count": 2,
          "kind": "nvme",
          "paths": []
        }
      },
      "mounts": [
        {
          "device": "/dev/mapper/nvme0-system",
          "format": "btrfs",
          "options": [
            "subvol=@root",
            "degraded"
          ],
          "path": "/"
        },
        {
          "device": "/dev/disk/by-partlabel/nvme0-esp",
          "format": "vfat",
          "options": [],
          "path": "/boot"
        },
        {
          "device": "/dev/mapper/nvme0-system",
          "format": "btrfs",
          "options": [
            "subvol=@nix",
            "noatime",
            "degraded"
          ],
          "path": "/nix"
        }
      ],
      "partitions": [
        {
          "device": 0,
          "disk": "nvme",
          "encrypt": false,
          "format": "vfat",
          "label": "nvme0-esp",
          "size": "1G"
        },
        {
          "device": 0,
          "disk": "nvme",
          "encrypt": true,
          "format": "",
          "label": "nvme0-system",
          "size": "rest"
        },
        {
          "device": 1,
          "disk": "nvme",
          "encrypt": true,
          "format": "",
          "label": "nvme1-system",
          "size": "rest"
        }
      ],
      "spanning": [
        {
          "degraded": "boot",
          "format": "btrfs",
          "members": [
            "/dev/mapper/nvme0-system",
            "/dev/mapper/nvme1-system"
          ],
          "name": "root",
          "raid": "raid1",
          "subvolumes": [
            "@nix",
            "@persist",
            "@root"
          ]
        }
      ],
      "unlock": {
        "pcrs": [
          7
        ],
        "ssh": {
          "authorized_keys": [],
          "port": 2222
        },
        "tpm2": false
      }
    }
    "#);

    let laptop = storage_of(
        "[disks.main]\ndevices.paths = [\"/dev/disk/by-id/nvme-laptop\"]\n\
         partitions.esp = { size = \"512M\", format = \"vfat\", mount = \"/boot\" }\n\
         partitions.swap = { size = \"16G\", format = \"swap\", encrypt = true }\n\
         partitions.system = { size = \"rest\", format = \"ext4\", mount = \"/\", encrypt = true }\n",
    )
    .unwrap();
    insta::assert_snapshot!(laptop, @r#"
    {
      "disks": {
        "main": {
          "at_least": "",
          "count": 1,
          "kind": "any",
          "paths": [
            "/dev/disk/by-id/nvme-laptop"
          ]
        }
      },
      "mounts": [
        {
          "device": "/dev/mapper/main0-swap",
          "format": "swap",
          "options": [],
          "path": ""
        },
        {
          "device": "/dev/mapper/main0-system",
          "format": "ext4",
          "options": [],
          "path": "/"
        },
        {
          "device": "/dev/disk/by-partlabel/main0-esp",
          "format": "vfat",
          "options": [],
          "path": "/boot"
        }
      ],
      "partitions": [
        {
          "device": 0,
          "disk": "main",
          "encrypt": false,
          "format": "vfat",
          "label": "main0-esp",
          "size": "512M"
        },
        {
          "device": 0,
          "disk": "main",
          "encrypt": true,
          "format": "swap",
          "label": "main0-swap",
          "size": "16G"
        },
        {
          "device": 0,
          "disk": "main",
          "encrypt": true,
          "format": "ext4",
          "label": "main0-system",
          "size": "rest"
        }
      ],
      "spanning": [],
      "unlock": {
        "pcrs": [
          7
        ],
        "ssh": {
          "authorized_keys": [],
          "port": 2222
        },
        "tpm2": false
      }
    }
    "#);
}

#[test]
fn a_raid_that_stops_without_a_disk_waits_for_all_of_them() {
    let stop = storage_of(
        "[disks.d]\ndevices.count = 2\npartitions.esp = { size = \"1G\", format = \"vfat\", \
         mount = \"/boot\", on = \"first\" }\npartitions.s = { size = \"rest\" }\n\n\
         [filesystems.root]\nraid = \"raid1\"\ndegraded = \"stop\"\non = [\"d.s\"]\nmount = \"/\"\n",
    )
    .unwrap();
    assert!(
        stop.contains("\"device=/dev/disk/by-partlabel/d1-s\""),
        "{stop}"
    );
    assert!(stop.contains("\"degraded\": \"stop\""), "{stop}");
    assert!(!stop.contains("\"degraded\"\n"), "{stop}");
}

#[test]
fn disks_say_what_doesnt_fit() {
    let errors: Vec<String> = [
        "[disks.a]\npartitions.x = { size = \"rest\", format = \"ext4\", mount = \"/\" }\n\
         partitions.y = { size = \"rest\" }\n",
        "[disks.a]\npartitions.x = { size = \"10 gigs\", format = \"ext4\", mount = \"/\" }\n",
        "[disks.a]\npartitions.x = { size = \"rest\", format = \"ext4\" }\n",
        "[disks.a]\npartitions.x = { size = \"rest\" }\n\n[filesystems.root]\non = [\"a.y\"]\n\
         mount = \"/\"\n",
        "[disks.a]\npartitions.x = { size = \"rest\" }\n\n[filesystems.root]\nraid = \"raid1\"\n\
         on = [\"a.x\"]\nmount = \"/\"\n",
        "[disks.a]\npartitions.x = { size = \"1G\", format = \"ext4\", mount = \"/srv\" }\n",
    ]
    .iter()
    .map(|system| storage_of(system).unwrap_err())
    .map(|error| error.lines().next().unwrap_or_default().to_owned())
    .collect();
    insta::assert_snapshot!(errors.join("\n"), @"
    error: only one partition can take the `rest`
    error: a size is like `1G`, or `rest`
    error: a partition with a filesystem needs a mount, and one with a mount needs its filesystem
    error: there's no partition `a.y`; partitions are named as `<disk>.<partition>`
    error: raid1 needs at least two partitions
    error: nothing is mounted at `/`
    ");
}

#[test]
fn base_modules_add_service_packages_enable_their_units_and_make_wrappers() {
    let evaluation = run(r#"
def render(ctx):
    ctx.service_package("dbus")
    ctx.enable("systemd-networkd.service")
    ctx.enable("pipewire.service", user = True, wanted_by = "default.target")
    ctx.wrapper("sudo", source = "/run/current-system/sw/bin/sudo")
    ctx.wrapper("ping", source = "/run/current-system/sw/bin/ping", group = "net", mode = 0o755)

module(name = "base-test", options = {}, writes = [], render = render, base = True)
"#)
    .unwrap();
    let output = output_of(&evaluation, "base-test");
    assert!(output.service_packages.contains("dbus") && output.packages.contains("dbus"));
    assert!(output.enabled["systemd-networkd.service"].is_empty());
    assert_eq!(
        output.enabled_user["pipewire.service"],
        ["default.target".to_owned()].into()
    );
    let sudo = &output.wrappers["sudo"];
    assert_eq!(
        (sudo.owner.as_str(), sudo.group.as_str(), sudo.mode),
        ("root", "root", 0o4755)
    );
    assert_eq!(output.wrappers["ping"].mode, 0o755);

    let message = run(r#"
def render(ctx):
    ctx.wrapper("bin/sudo", source = "sudo")

module(name = "bad", options = {}, writes = [], render = render)
"#)
    .unwrap_err();
    assert!(message.contains("absolute path"), "{message}");
}

const UNIT_MODULES: [&str; 1] = ["units"];

fn with_units(toml: &str) -> MemFs {
    MemFs::new([("system.toml", toml)])
}

#[test]
fn units_are_written_in_toml_and_timers_start_on_their_own() {
    let config = with_units(
        r#"
[units."backup.service"]
Unit.Description = "Nightly backup"
Service = { Type = "oneshot", ExecStart = "restic backup /home" }

[units."backup.timer"]
Timer.OnCalendar = "daily"

[units."manual.timer"]
Timer.OnCalendar = "weekly"
Install.WantedBy = "custom.target"

[units."nginx.service.d/limits.conf"]
Service.LimitNOFILE = 4096
"#,
    );

    let evaluation = evaluate_with(&config, &UNIT_MODULES).unwrap();
    insta::assert_snapshot!(rendered_by(&evaluation, "units"), @"
    backup.service
    [Unit]
    Description=Nightly backup

    [Service]
    ExecStart=restic backup /home
    Type=oneshot

    backup.timer
    [Timer]
    OnCalendar=daily

    [Install]
    WantedBy=timers.target

    manual.timer
    [Timer]
    OnCalendar=weekly

    [Install]
    WantedBy=custom.target

    nginx.service.d/limits.conf
    [Service]
    LimitNOFILE=4096
    ");
}

#[test]
fn a_unit_needs_a_name_systemd_knows_and_sections() {
    for (toml, expected) in [
        (
            "[units.backup]\nService.ExecStart = \"x\"\n",
            "`backup` isn't a unit name",
        ),
        (
            "[units.\"backup.service\"]\nExecStart = \"x\"\n",
            "`ExecStart` isn't a section",
        ),
    ] {
        let error = evaluate_with(&with_units(toml), &UNIT_MODULES).unwrap_err();
        assert!(error.contains(expected), "{error}");
        assert!(error.contains("system.toml"), "{error}");
    }
}

const BUILD_MODULES: [&str; 1] = ["builds"];

#[test]
fn builds_say_what_differs_and_hold_their_patches() {
    let config = MemFs::new([
        (
            "system.toml",
            r#"
[builds.whisper-vulkan]
package = "whisper-cpp"
options.vulkanSupport = true
patches = ["patches/fix.patch"]

[builds.hello]
patches = ["patches/fix.patch"]
"#,
        ),
        ("patches/fix.patch", "--- a\n+++ b\n"),
    ]);

    let evaluation = evaluate_with(&config, &BUILD_MODULES).unwrap();
    let builds = &output_of(&evaluation, "builds").builds;
    assert_eq!(builds["whisper-vulkan"].package, "whisper-cpp");
    assert_eq!(builds["whisper-vulkan"].options.len(), 1);
    assert_eq!(builds["hello"].package, "hello");
    assert_eq!(evaluation.patches()["patches/fix.patch"], "--- a\n+++ b\n");
}

#[test]
fn a_build_needs_its_patches() {
    let config = MemFs::new([(
        "system.toml",
        "[builds.hello]\npatches = [\"gone.patch\"]\n",
    )]);

    let error = evaluate_with(&config, &BUILD_MODULES).unwrap_err();
    assert!(
        error.contains("cannot read patch `gone.patch` of build `hello`"),
        "{error}"
    );
}

#[test]
fn the_kernel_module_writes_settings_and_leaves_the_boot_to_the_backend() {
    let config = MemFs::new([(
        "system.toml",
        r#"
[kernel]
package = "linuxPackages_latest"
modules = ["kvm-amd"]
initrd_modules = ["nvme"]
blacklist = ["pcspkr"]
module_options.nbd = "nbds_max=2"
sysctl."vm.swappiness" = 133

[hardware]
firmware = ["linux-firmware"]
microcode = "amd"
"#,
    )]);

    let evaluation = evaluate_with(&config, &["kernel"]).unwrap();
    insta::assert_snapshot!(rendered_by(&evaluation, "kernel"), @r#"
    /etc/modprobe.d/distro.conf
    blacklist pcspkr
    options nbd nbds_max=2

    /etc/modules-load.d/kernel.conf
    kvm-amd

    /etc/sysctl.d/60-kernel.conf
    vm.swappiness = 133

    restart systemd-modules-load.service on {"/etc/modules-load.d/kernel.conf"}
    restart systemd-sysctl.service on {"/etc/sysctl.d/60-kernel.conf"}
    "#);
    let base = &output_of(&evaluation, "kernel").base["kernel"];
    insta::assert_snapshot!(base.to_string(), @r#"{ firmware = ["linux-firmware"], initrd_modules = ["nvme"], microcode = { image = "amd-ucode.img", package = "microcode-amd" }, modprobe = "blacklist pcspkr\noptions nbd nbds_max=2\n", package = "linuxPackages_latest" }"#);
}
