use serde::Deserialize;

use crate::transport::Transport;

#[derive(Debug, Clone)]
pub struct ContainerInfo {
    pub id: String,
    pub name: String,
    pub image: String,
    pub status: String,
    pub running: bool,
    pub candidate: bool,
}

/// `docker ps -a --format '{{json .}}'` emits one PascalCase JSON object
/// per line.
#[derive(Deserialize)]
struct PsRow {
    #[serde(rename = "ID")]
    id: String,
    #[serde(rename = "Names")]
    names: String,
    #[serde(rename = "Image")]
    image: String,
    #[serde(rename = "Status")]
    status: String,
    #[serde(rename = "State")]
    state: String,
}

/// AmneziaWG images are the adopt candidates; everything else is listed for
/// context. Final say belongs to `awg-mesh adopt --inspect`, which fails
/// cleanly on containers without /opt/amnezia/awg/awg0.conf.
pub fn is_candidate(image: &str) -> bool {
    image.to_lowercase().contains("amneziawg")
}

pub fn parse_ps(stdout: &str) -> Vec<ContainerInfo> {
    stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<PsRow>(l).ok())
        .map(|r| ContainerInfo {
            id: r.id,
            name: r.names,
            running: r.state == "running",
            candidate: is_candidate(&r.image),
            image: r.image,
            status: r.status,
        })
        .collect()
}

pub const COMPOSE_PROJECT: &str = "amnezia-mesh";

pub async fn discover(transport: &Transport, docker: &str) -> Result<Vec<ContainerInfo>, String> {
    let out = transport
        .exec(&format!("{docker} ps -a --format '{{{{json .}}}}'"))
        .await
        .map_err(|e| e.to_string())?;
    if !out.ok() {
        return Err(format!("docker ps failed: {}", out.stderr.trim()));
    }
    Ok(parse_ps(&out.stdout))
}

/// Containers of the initialized amnezia-mesh stack (compose project label —
/// no compose plugin needed on the server).
pub async fn stack_services(
    transport: &Transport,
    docker: &str,
) -> Result<Vec<ContainerInfo>, String> {
    let out = transport
        .exec(&format!(
            "{docker} ps -a --filter label=com.docker.compose.project={COMPOSE_PROJECT} \
             --format '{{{{json .}}}}'"
        ))
        .await
        .map_err(|e| e.to_string())?;
    if !out.ok() {
        return Err(format!("docker ps failed: {}", out.stderr.trim()));
    }
    Ok(parse_ps(&out.stdout))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ps_marks_amnezia_images_as_candidates() {
        let stdout = concat!(
            r#"{"Command":"x","ID":"9d041a65c06e","Image":"amneziavpn/amneziawg-go:latest","Names":"amnezia-legacy-test","State":"running","Status":"Up 3 minutes"}"#,
            "\n",
            r#"{"Command":"x","ID":"e867f97f7085","Image":"d2957c8b3094","Names":"some_api","State":"exited","Status":"Exited (1) ago"}"#,
            "\n",
            "not-json-at-all\n",
        );
        let cs = parse_ps(stdout);
        assert_eq!(cs.len(), 2);
        assert!(cs[0].candidate && cs[0].running);
        assert_eq!(cs[0].name, "amnezia-legacy-test");
        assert!(!cs[1].candidate && !cs[1].running);
    }

    #[test]
    fn candidate_check_is_case_insensitive() {
        assert!(is_candidate("AmneziaWG-go:latest"));
        assert!(is_candidate("amneziawg-go"));
        assert!(!is_candidate("nginx:alpine"));
    }
}
