use std::{
    collections::HashMap,
    error::Error,
    fs::File,
    io::Read,
    os::unix::process::{CommandExt, ExitStatusExt},
    process::{Child, Command, Stdio},
    sync::{atomic::AtomicBool, Arc, LazyLock, Mutex},
    thread::sleep,
    time::Duration,
};

use clap::Parser;
use serde::{Deserialize, Serialize};

#[derive(Debug, Parser)]
#[command()]
struct Args {
    #[arg(short)]
    config: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct App {
    name: Option<String>,
    path: String,
    args: Option<Vec<String>>,
    env: Option<HashMap<String, String>>,
    restart: Option<bool>,
    stdout: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct Config {
    interval: Option<u64>,
    #[serde(rename = "app")]
    apps: Vec<App>,
}

const DEFAULT_CONFIG_PATH: &str = ".spawner.toml";
static HANDBRAKE: LazyLock<Arc<AtomicBool>> = LazyLock::new(|| Arc::new(AtomicBool::new(false)));

struct Cmd<'a> {
    command: Arc<Mutex<Command>>,
    child: Arc<Mutex<Option<Child>>>,
    app: &'a App,
}

impl<'a> Cmd<'a> {
    fn new(command: Command, child: Child, app: &'a App) -> Self {
        Self {
            command: Arc::new(Mutex::new(command)),
            child: Arc::new(Mutex::new(Some(child))),
            app,
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    println!("parsed args: {:#?}", args);

    let config = parse(&args)?;
    println!("read conf:\n{}", toml::to_string_pretty(&config)?);

    setup().unwrap();

    let cmds = start(&config)?;
    println!("spawned {} apps", cmds.len());

    behold(&config, &cmds)?;

    Ok(())
}

fn setup() -> Result<(), Box<dyn Error>> {
    ctrlc::set_handler(move || {
        HANDBRAKE.store(true, std::sync::atomic::Ordering::SeqCst);
    })
    .unwrap();

    Ok(())
}

fn parse(Args { config, .. }: &Args) -> Result<Config, Box<dyn Error>> {
    let mut input = File::open(config.as_ref().map_or(DEFAULT_CONFIG_PATH, |x| x.as_str()))?;

    let mut config = String::new();
    input.read_to_string(&mut config)?;
    let x: Config = toml::from_str(&config)?;

    Ok(x)
}

fn start(x: &Config) -> Result<Vec<Cmd>, Box<dyn Error>> {
    let mut cmds = vec![];

    for app in x.apps.iter() {
        let mut command = Command::new(&app.path);
        if let Some(out) = &app.stdout {
            let f = File::create(out)?;
            command.stdout(Stdio::from(f));
        }

        if let Some(args) = &app.args {
            command.args(args);
        }

        if let Some(env) = &app.env {
            command.envs(
                env.iter()
                    .map(|(k, v)| (k, shellexpand::env(v).unwrap().to_string())),
            );
        }

        if let Some(name) = &app.name {
            command.arg0(format!("[spawner: {name}] -> {}", &app.path));
        }

        let handle = command.spawn()?;
        println!("starting {} with PID: {}", app.path, handle.id());

        cmds.push(Cmd::new(command, handle, app));
    }

    Ok(cmds)
}

fn behold(config: &Config, cmds: &Vec<Cmd>) -> Result<(), Box<dyn Error>> {
    let interval = config.interval.map_or(5000, |x| x * 1000);
    loop {
        if HANDBRAKE.load(std::sync::atomic::Ordering::SeqCst) {
            println!("exit triggered from ctrlc");
            for Cmd { child, .. } in cmds.iter() {
                let mut child = child.lock().unwrap();
                if let Some(child) = child.as_mut() {
                    child.kill().unwrap();
                }
            }

            break;
        }

        sleep(Duration::from_millis(interval));

        for Cmd {
            command,
            child,
            app,
        } in cmds.iter()
        {
            let command = command.clone();
            let child = child.clone();
            let mut child = child.lock().unwrap();
            let mut command = command.lock().unwrap();

            let (running, restart) =
                child
                    .as_mut()
                    .map_or((false, false), |x| match x.try_wait() {
                        Ok(Some(x)) => match x.code() {
                            Some(code) => match code {
                                0 => {
                                    println!("exited without error");
                                    (false, false)
                                }
                                code => {
                                    println!("exited with code: {code}");
                                    (false, true)
                                }
                            },
                            None => {
                                println!(
                                    "killed by signal: {}",
                                    x.signal().map_or("unknown".to_string(), |s| s.to_string())
                                );
                                (false, true)
                            }
                        },
                        Ok(None) => {
                            // println!("still running");
                            (true, false)
                        }
                        Err(e) => panic!("{}", e),
                    });

            if !running {
                child.take();
            }

            if restart && app.restart.unwrap_or(true) {
                let new = command.spawn()?;
                child.replace(new);
            }

            println!(
                "{} {}",
                command.get_program().to_str().unwrap(),
                if running {
                    format!("running as PID: {}", child.as_ref().map_or(0, |x| x.id()))
                } else {
                    format!("not running")
                }
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load() {
        let x: Config = toml::from_str(
            r#"
        [[app]]
        name = "some"
        path = "/bin/sh"
        args = ["-c", "echo", "hola"]
    
        [[app]]
        path = "/bin/sleep"
        env = { PATH = "b", c = "d"}
        "#,
        )
        .unwrap();

        let app = App {
            path: "halt".to_string(),
            env: Some(HashMap::from_iter([(
                "PATH".to_string(),
                "/sbin".to_string(),
            )])),
            ..Default::default()
        };

        let apps = Config {
            apps: vec![app],
            ..Default::default()
        };

        dbg!(x);
        println!("{}", toml::to_string_pretty(&apps).unwrap());
    }
}
