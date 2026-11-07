use std::collections::BTreeMap;
use std::path::Path;

use distro_eval::{
    ConfigFs, DirFs, Evaluation, Input, MemFs, Module, OptionPath, OptionSpec, Output, Schema,
    Sources, Type, Value, describe_outputs, evaluate, explain,
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
        role: None,
        schema: Schema::default(),
        modules: &modules,
        facts: &Value::Table(BTreeMap::new()),
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
        role: None,
        schema: base_schema(),
        modules: &[module],
        facts: &Value::Table(BTreeMap::new()),
    })
    .map_err(|e| e.to_string())
}

#[test]
fn the_built_in_modules_render_the_fixture() {
    let evaluation = evaluate_fixture("s4", &MODULES);

    insta::assert_snapshot!("s4_outputs", describe_outputs(&evaluation));
    let why = |path: &str| explain(&evaluation, Some(&OptionPath::parse(path).unwrap())).unwrap();
    insta::assert_snapshot!(why("security.pam.services.sudo.auth"), @r#"
    security.pam.services.sudo.auth = [{ control = "required", module = "pam_env.so", name = "env" }, { control = "sufficient", module = "pam_fprintd.so", name = "fprintd" }, { args = ["likeauth", "try_first_pass"], control = "sufficient", module = "pam_unix.so", name = "unix" }]
      set by module `fprintd` at modules/fprintd.star:14:9: [{ after = "env", before = "unix", control = "sufficient", module = "pam_fprintd.so", name = "fprintd" }]
      set by module `pam-base` at modules/pam-base.star:31:9: [{ control = "required", module = "pam_env.so", name = "env" }, { after = "env", args = ["likeauth", "try_first_pass"], control = "sufficient", module = "pam_unix.so", name = "unix" }, { after = "unix", control = "required", module = "pam_deny.so", name = "deny" }]
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
        role: None,
        schema: base_schema(),
        modules: &modules,
        facts: &Value::Table(BTreeMap::new()),
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
fn session_variables_reach_systemd_and_login_shells() {
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
fn session_variables_cannot_run_commands() {
    let config = MemFs::new([
        ("system.toml", "[system]\nhostname = \"box\"\n"),
        ("users/john.toml", "session_variables.X = \"$(id)\"\n"),
    ]);

    let error = evaluate_with(&config, &HOME_MODULES).unwrap_err();
    assert!(error.contains("users.john.session_variables.X"), "{error}");
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
        role: None,
        schema: base_schema(),
        modules: &[module],
        facts: &Value::Table(BTreeMap::new()),
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
