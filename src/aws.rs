//! Thin wrapper around the `aws` CLI. Mirrors the calls made by
//! `shared/clients/ecsClient.ts` in aws-toolkit-vscode, but shells out so the
//! tool shares the user's profiles, SSO sessions and credential cache.

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;

#[derive(Debug, Clone)]
pub struct Profile {
    pub name: String,
    pub region: Option<String>,
    pub login: Option<Login>,
}

/// How a profile signs in, taken from its keys in ~/.aws/config.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Login {
    /// `sso_session` or legacy `sso_start_url`: IAM Identity Center.
    Sso,
    /// `login_session`: console credentials from `aws login`.
    Console,
}

impl Login {
    pub fn command(self, profile: &str) -> String {
        match self {
            Login::Sso => format!("aws sso login --profile {profile}"),
            Login::Console => format!("aws login --profile {profile}"),
        }
    }
}

/// Whether an `aws` error means the session expired or its credentials were
/// rejected, so logging in again can help. The CLI's credential errors all
/// mention one of these; network failures (offline, an unknown region) name
/// the "endpoint URL" instead, even when that URL is the SSO token endpoint.
pub fn is_expired_session(err: &str) -> bool {
    let err = err.to_ascii_lowercase();
    !err.contains("endpoint url") && ["expired", "token", "aws login"].iter().any(|m| err.contains(m))
}

fn aws_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".aws"))
}

/// Reads profile names from ~/.aws/config and ~/.aws/credentials.
pub fn load_profiles() -> Vec<Profile> {
    aws_dir().map(|d| load_profiles_from(&d)).unwrap_or_default()
}

fn load_profiles_from(dir: &std::path::Path) -> Vec<Profile> {
    let mut profiles: Vec<Profile> = Vec::new();

    let mut push = |name: &str, region: Option<String>, login: Option<Login>| {
        if let Some(p) = profiles.iter_mut().find(|p| p.name == name) {
            if p.region.is_none() {
                p.region = region;
            }
            if p.login.is_none() {
                p.login = login;
            }
        } else {
            profiles.push(Profile { name: name.to_string(), region, login });
        }
    };

    for (file, prefixed) in [("config", true), ("credentials", false)] {
        let Ok(text) = std::fs::read_to_string(dir.join(file)) else { continue };
        let mut current: Option<String> = None;
        let mut region: Option<String> = None;
        let mut login: Option<Login> = None;
        let mut flush = |current: &mut Option<String>, region: &mut Option<String>, login: &mut Option<Login>| {
            if let Some(name) = current.take() {
                push(&name, region.take(), login.take());
            }
        };
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            if let Some(header) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                flush(&mut current, &mut region, &mut login);
                // Keys under a skipped section must not leak into the next profile.
                region = None;
                login = None;
                let header = header.trim();
                let name = if prefixed {
                    match header.strip_prefix("profile ") {
                        Some(n) => n.trim(),
                        None if header == "default" => header,
                        // [sso-session x], [services x] etc. are not profiles
                        None => continue,
                    }
                } else {
                    header
                };
                current = Some(name.to_string());
            } else if let Some((k, v)) = line.split_once('=') {
                match k.trim() {
                    "region" => region = Some(v.trim().to_string()),
                    "sso_session" | "sso_start_url" if prefixed => login = Some(Login::Sso),
                    "login_session" if prefixed => login = Some(Login::Console),
                    _ => {}
                }
            }
        }
        flush(&mut current, &mut region, &mut login);
    }

    profiles.sort_by(|a, b| (a.name != "default").cmp(&(b.name != "default")).then(a.name.cmp(&b.name)));
    profiles
}

#[derive(Debug, Clone)]
pub struct Cluster {
    pub arn: String,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct Service {
    pub arn: String,
    pub name: String,
    pub status: String,
    pub cluster_arn: String,
    pub task_definition: String,
    pub enable_execute_command: bool,
}

#[derive(Debug, Clone)]
pub struct TaskDefinition {
    pub containers: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Task {
    pub arn: String,
    pub last_status: String,
    pub desired_status: String,
    /// container name -> has ExecuteCommandAgent
    pub containers: Vec<(String, bool)>,
}

impl Task {
    /// Last 32 chars of the ARN, as the reference wizard does.
    pub fn id(&self) -> &str {
        self.arn.rsplit('/').next().unwrap_or(&self.arn)
    }
    /// `createValidTaskFilter` from model.ts
    pub fn exec_ready_for(&self, container: &str) -> bool {
        self.containers.iter().any(|(n, agent)| n == container && *agent)
    }
}

#[derive(Debug, Clone)]
pub struct Ecs {
    pub profile: String,
    pub region: String,
}

fn s(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
}

impl Ecs {
    pub fn prefix(&self) -> String {
        format!("--profile {} --region {}", self.profile, self.region)
    }

    /// Cheap call that fails when the profile has no usable credentials,
    /// e.g. an expired SSO token.
    pub fn check_credentials(&self) -> Result<()> {
        let out = Command::new("aws")
            .args(["--profile", &self.profile, "--region", &self.region, "sts", "get-caller-identity"])
            .output()
            .context("failed to launch `aws` CLI; is it installed and on PATH?")?;
        if !out.status.success() {
            bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(())
    }

    fn run(&self, args: &[&str]) -> Result<Value> {
        let out = Command::new("aws")
            .args(["--profile", &self.profile, "--region", &self.region, "--output", "json", "ecs"])
            .args(args)
            .output()
            .context("failed to launch `aws` CLI; is it installed and on PATH?")?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            bail!("aws ecs {}: {}", args.first().unwrap_or(&""), err.trim());
        }
        if out.stdout.iter().all(u8::is_ascii_whitespace) {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&out.stdout).context("aws CLI returned invalid JSON")
    }

    fn arns(v: &Value, key: &str) -> Vec<String> {
        v.get(key)
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
            .unwrap_or_default()
    }

    pub fn list_clusters(&self) -> Result<Vec<Cluster>> {
        let arns = Self::arns(&self.run(&["list-clusters"])?, "clusterArns");
        let mut clusters = Vec::new();
        for chunk in arns.chunks(100) {
            let mut args = vec!["describe-clusters", "--clusters"];
            args.extend(chunk.iter().map(String::as_str));
            let v = self.run(&args)?;
            for c in v.get("clusters").and_then(Value::as_array).into_iter().flatten() {
                clusters.push(Cluster { arn: s(c, "clusterArn"), name: s(c, "clusterName") });
            }
        }
        clusters.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(clusters)
    }

    pub fn list_services(&self, cluster: &str) -> Result<Vec<Service>> {
        let arns = Self::arns(&self.run(&["list-services", "--cluster", cluster])?, "serviceArns");
        let mut services = Vec::new();
        for chunk in arns.chunks(10) {
            let mut args = vec!["describe-services", "--cluster", cluster, "--services"];
            args.extend(chunk.iter().map(String::as_str));
            let v = self.run(&args)?;
            for sv in v.get("services").and_then(Value::as_array).into_iter().flatten() {
                services.push(Service {
                    arn: s(sv, "serviceArn"),
                    name: s(sv, "serviceName"),
                    status: s(sv, "status"),
                    cluster_arn: s(sv, "clusterArn"),
                    task_definition: s(sv, "taskDefinition"),
                    enable_execute_command: sv
                        .get("enableExecuteCommand")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                });
            }
        }
        services.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(services)
    }

    pub fn describe_task_definition(&self, task_definition: &str) -> Result<TaskDefinition> {
        let v = self.run(&["describe-task-definition", "--task-definition", task_definition])?;
        let td = v.get("taskDefinition").ok_or_else(|| anyhow!("no taskDefinition in response"))?;
        let containers = td
            .get("containerDefinitions")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|c| s(c, "name"))
            .collect();
        Ok(TaskDefinition { containers })
    }

    pub fn list_tasks(&self, cluster: &str, service: &str) -> Result<Vec<Task>> {
        let arns = Self::arns(
            &self.run(&["list-tasks", "--cluster", cluster, "--service-name", service])?,
            "taskArns",
        );
        let mut tasks = Vec::new();
        for chunk in arns.chunks(100) {
            let mut args = vec!["describe-tasks", "--cluster", cluster, "--tasks"];
            args.extend(chunk.iter().map(String::as_str));
            let v = self.run(&args)?;
            for t in v.get("tasks").and_then(Value::as_array).into_iter().flatten() {
                let containers = t
                    .get("containers")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .map(|c| {
                        let agent = c
                            .get("managedAgents")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                            .any(|a| s(a, "name") == "ExecuteCommandAgent");
                        (s(c, "name"), agent)
                    })
                    .collect();
                tasks.push(Task {
                    arn: s(t, "taskArn"),
                    last_status: s(t, "lastStatus"),
                    desired_status: s(t, "desiredStatus"),
                    containers,
                });
            }
        }
        // RUNNING first, like the reference quick pick's `compare`
        tasks.sort_by_key(|t| t.last_status != "RUNNING");
        Ok(tasks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_config_and_credentials() {
        let dir = std::env::temp_dir().join(format!("ecsterm-test-{}", std::process::id()));
        std::fs::create_dir_all(dir.join(".aws")).unwrap();
        std::fs::write(
            dir.join(".aws/config"),
            "[default]\nregion = eu-west-1\nlogin_session = arn:aws:sts::1:assumed-role/x/y\n\n[profile prod]\nregion=eu-north-1\nsso_session = corp\n\n[sso-session corp]\nsso_region = us-east-1\nsso_start_url = https://x\n\n[profile plain]\nregion = eu-west-2\n",
        )
        .unwrap();
        std::fs::write(dir.join(".aws/credentials"), "[prod]\naws_access_key_id = x\n[legacy]\naws_access_key_id = y\n").unwrap();
        let profiles = load_profiles_from(&dir.join(".aws"));
        let _ = std::fs::remove_dir_all(&dir);

        let names: Vec<_> = profiles.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["default", "legacy", "plain", "prod"]);
        assert_eq!(profiles[3].region.as_deref(), Some("eu-north-1"));
        assert_eq!(profiles[1].region, None);
        assert_eq!(profiles[0].login, Some(Login::Console));
        assert_eq!(profiles[1].login, None);
        assert_eq!(profiles[2].login, None);
        assert_eq!(profiles[3].login, Some(Login::Sso));
    }

    #[test]
    fn expired_session_errors_are_told_apart_from_unreachable_endpoints() {
        for expired in [
            "Error when retrieving token from sso: Token has expired and refresh failed",
            "Error loading SSO Token: Token for corp does not exist",
            "The SSO session associated with this profile has expired or is otherwise invalid. To refresh this SSO session run aws sso login with the corresponding profile.",
            "Your session has expired. Please reauthenticate using 'aws login'.",
            "Unable to refresh login credentials because of a change in your password. Please reauthenticate with your new password using 'aws login'.",
            "An error occurred (ExpiredToken) when calling the GetCallerIdentity operation: The security token included in the request is expired",
            "An error occurred (UnauthorizedException) when calling the GetRoleCredentials operation: Session token not found or invalid",
        ] {
            assert!(is_expired_session(expired), "{expired}");
        }
        for other in [
            "Could not connect to the endpoint URL: \"https://sts.eu-west-11.amazonaws.com/\"",
            "Could not connect to the endpoint URL: \"https://oidc.eu-west-1.amazonaws.com/token\"",
            "Connect timeout on endpoint URL: \"https://portal.sso.eu-west-1.amazonaws.com/federation/credentials\"",
            "An error occurred (Throttling) when calling the GetCallerIdentity operation: Rate exceeded",
            "The config profile (nope) could not be found",
            "failed to launch `aws` CLI; is it installed and on PATH?",
        ] {
            assert!(!is_expired_session(other), "{other}");
        }
    }

    #[test]
    fn task_id_and_exec_filter() {
        let t = Task {
            arn: "arn:aws:ecs:eu-west-1:123:task/mila-prod/0123456789abcdef0123456789abcdef".into(),
            last_status: "RUNNING".into(),
            desired_status: "RUNNING".into(),
            containers: vec![("netbox".into(), true), ("sidecar".into(), false)],
        };
        assert_eq!(t.id(), "0123456789abcdef0123456789abcdef");
        assert!(t.exec_ready_for("netbox"));
        assert!(!t.exec_ready_for("sidecar"));
    }
}
