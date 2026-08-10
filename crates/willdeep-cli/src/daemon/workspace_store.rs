use super::*;

const WORKSPACE_SCHEMA: u32 = 1;
const MAX_WORKSPACE_NAME_CHARS: usize = 120;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WorkspaceAccess {
    ReadOnly,
    #[default]
    WorkspaceWrite,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct RuntimeWorkspace {
    pub schema: u32,
    pub id: uuid::Uuid,
    pub name: String,
    pub root: PathBuf,
    pub access: WorkspaceAccess,
    pub provider_profile: Option<String>,
    pub skills: Vec<String>,
    pub mcp_servers: Vec<String>,
    pub created_at: u64,
    pub updated_at: u64,
    pub active: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct RegisterWorkspace {
    pub root: PathBuf,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub access: WorkspaceAccess,
    #[serde(default)]
    pub provider_profile: Option<String>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub mcp_servers: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct PersistedWorkspaces {
    schema: u32,
    active_id: Option<uuid::Uuid>,
    items: Vec<RuntimeWorkspace>,
}

pub(super) struct WorkspaceStore {
    path: PathBuf,
    state: Mutex<PersistedWorkspaces>,
}

impl WorkspaceStore {
    pub fn open(path: PathBuf) -> Result<Self> {
        let state = if path.exists() {
            let content = std::fs::read(&path)
                .with_context(|| format!("read Workspace registry {}", path.display()))?;
            let state: PersistedWorkspaces = serde_json::from_slice(&content)
                .with_context(|| format!("parse Workspace registry {}", path.display()))?;
            if state.schema != WORKSPACE_SCHEMA {
                bail!("unsupported Workspace registry schema {}", state.schema);
            }
            state
        } else {
            PersistedWorkspaces {
                schema: WORKSPACE_SCHEMA,
                ..PersistedWorkspaces::default()
            }
        };
        Ok(Self {
            path,
            state: Mutex::new(state),
        })
    }

    pub fn list(&self) -> Result<Vec<RuntimeWorkspace>> {
        let state = self.lock()?;
        let mut items = state.items.clone();
        mark_active(&mut items, state.active_id);
        items.sort_by(|left, right| {
            right
                .active
                .cmp(&left.active)
                .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
        });
        Ok(items)
    }

    pub fn get(&self, id: uuid::Uuid) -> Result<Option<RuntimeWorkspace>> {
        let state = self.lock()?;
        let mut item = state.items.iter().find(|item| item.id == id).cloned();
        if let Some(item) = item.as_mut() {
            item.active = state.active_id == Some(id);
        }
        Ok(item)
    }

    pub fn active(&self) -> Result<Option<RuntimeWorkspace>> {
        let state = self.lock()?;
        let Some(id) = state.active_id else {
            return Ok(None);
        };
        let mut item = state.items.iter().find(|item| item.id == id).cloned();
        if let Some(item) = item.as_mut() {
            item.active = true;
        }
        Ok(item)
    }

    pub fn register(&self, request: RegisterWorkspace) -> Result<(RuntimeWorkspace, bool)> {
        let root = canonical_directory(&request.root)?;
        let name = normalize_name(request.name, &root)?;
        let provider_profile = normalize_optional(request.provider_profile);
        let skills = normalize_names(request.skills, "Skill")?;
        let mcp_servers = normalize_names(request.mcp_servers, "MCP server")?;
        let timestamp = now();
        let mut state = self.lock()?;
        if let Some(index) = state.items.iter().position(|item| item.root == root) {
            let id = state.items[index].id;
            let active = state.active_id == Some(id);
            {
                let existing = &mut state.items[index];
                existing.name = name;
                existing.access = request.access;
                existing.provider_profile = provider_profile;
                existing.skills = skills;
                existing.mcp_servers = mcp_servers;
                existing.updated_at = timestamp;
                existing.active = active;
            }
            persist(&self.path, &state)?;
            return Ok((state.items[index].clone(), false));
        }
        let id = uuid::Uuid::new_v4();
        if state.active_id.is_none() {
            state.active_id = Some(id);
        }
        let item = RuntimeWorkspace {
            schema: WORKSPACE_SCHEMA,
            id,
            name,
            root,
            access: request.access,
            provider_profile,
            skills,
            mcp_servers,
            created_at: timestamp,
            updated_at: timestamp,
            active: state.active_id == Some(id),
        };
        state.items.push(item.clone());
        persist(&self.path, &state)?;
        Ok((item, true))
    }

    pub fn ensure_registered(&self, root: &Path) -> Result<RuntimeWorkspace> {
        let root = canonical_directory(root)?;
        if let Some(mut item) = self
            .lock()?
            .items
            .iter()
            .find(|item| item.root == root)
            .cloned()
        {
            item.active = self.active()?.is_some_and(|active| active.id == item.id);
            return Ok(item);
        }
        Ok(self
            .register(RegisterWorkspace {
                root,
                name: None,
                access: WorkspaceAccess::WorkspaceWrite,
                provider_profile: None,
                skills: Vec::new(),
                mcp_servers: Vec::new(),
            })?
            .0)
    }

    pub fn activate(&self, id: uuid::Uuid) -> Result<Option<RuntimeWorkspace>> {
        let mut state = self.lock()?;
        let Some(index) = state.items.iter().position(|item| item.id == id) else {
            return Ok(None);
        };
        state.active_id = Some(id);
        state.items[index].updated_at = now();
        persist(&self.path, &state)?;
        let mut item = state.items[index].clone();
        item.active = true;
        Ok(Some(item))
    }

    pub fn remove(&self, id: uuid::Uuid) -> Result<Option<RuntimeWorkspace>> {
        let mut state = self.lock()?;
        let Some(index) = state.items.iter().position(|item| item.id == id) else {
            return Ok(None);
        };
        let item = state.items.remove(index);
        if state.active_id == Some(id) {
            state.active_id = state.items.first().map(|item| item.id);
        }
        persist(&self.path, &state)?;
        Ok(Some(item))
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, PersistedWorkspaces>> {
        self.state
            .lock()
            .map_err(|_| anyhow::anyhow!("Workspace registry lock poisoned"))
    }
}

pub(super) async fn list_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    state
        .workspaces
        .list()
        .map(Json)
        .map(IntoResponse::into_response)
        .map_err(|error| {
            eprintln!("list Runtime Workspaces: {error:#}");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

pub(super) async fn register_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(request): Json<RegisterWorkspace>,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    let (workspace, created) = state.workspaces.register(request).map_err(|error| {
        eprintln!("register Runtime Workspace: {error:#}");
        StatusCode::BAD_REQUEST
    })?;
    state
        .events
        .append(
            if created {
                "workspace.registered"
            } else {
                "workspace.updated"
            },
            format!(
                "workspace_id={} root={}",
                workspace.id,
                workspace.root.display()
            ),
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok((
        if created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        Json(workspace),
    )
        .into_response())
}

pub(super) async fn get_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<uuid::Uuid>,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    state
        .workspaces
        .get(id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(Json)
        .map(IntoResponse::into_response)
        .ok_or(StatusCode::NOT_FOUND)
}

pub(super) async fn activate_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<uuid::Uuid>,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    let workspace = state
        .workspaces
        .activate(id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    state
        .events
        .append(
            "workspace.activated",
            format!(
                "workspace_id={} root={}",
                workspace.id,
                workspace.root.display()
            ),
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(workspace).into_response())
}

pub(super) async fn remove_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<uuid::Uuid>,
) -> Result<Response, StatusCode> {
    authorize(&state, &headers)?;
    let workspace = state
        .workspaces
        .remove(id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    state
        .events
        .append(
            "workspace.removed",
            format!(
                "workspace_id={} root={}",
                workspace.id,
                workspace.root.display()
            ),
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

pub(super) async fn register_cli(
    home: &Path,
    root: PathBuf,
    name: Option<String>,
    access: WorkspaceAccess,
    provider_profile: Option<String>,
    skills: Vec<String>,
    mcp_servers: Vec<String>,
) -> Result<()> {
    let state = ensure_running(home).await?;
    let response = client()
        .post(format!("http://{}/v1/workspaces", state.address))
        .header(TOKEN_HEADER, &state.token)
        .json(&RegisterWorkspace {
            root,
            name,
            access,
            provider_profile,
            skills,
            mcp_servers,
        })
        .send()
        .await?;
    if !response.status().is_success() {
        bail!(
            "Runtime rejected Workspace registration: {}",
            response.text().await?
        );
    }
    print_workspace(&response.json().await?);
    Ok(())
}

pub(super) async fn list_cli(home: &Path) -> Result<()> {
    let state = ensure_running(home).await?;
    let workspaces: Vec<RuntimeWorkspace> = authorized_get(&state, "/v1/workspaces").await?;
    for workspace in workspaces {
        print_workspace(&workspace);
    }
    Ok(())
}

pub(super) async fn resolve_cli_root(home: &Path, root: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(root) = root {
        return root
            .canonicalize()
            .with_context(|| format!("invalid Workspace root: {}", root.display()));
    }
    let state = ensure_running(home).await?;
    let workspaces: Vec<RuntimeWorkspace> = authorized_get(&state, "/v1/workspaces").await?;
    if let Some(active) = workspaces.into_iter().find(|workspace| workspace.active) {
        return Ok(active.root);
    }
    std::env::current_dir()?.canonicalize().map_err(Into::into)
}

pub(super) async fn activate_cli(home: &Path, id: uuid::Uuid) -> Result<()> {
    let state = ensure_running(home).await?;
    let response = client()
        .post(format!(
            "http://{}/v1/workspaces/{id}/activate",
            state.address
        ))
        .header(TOKEN_HEADER, &state.token)
        .send()
        .await?;
    if !response.status().is_success() {
        bail!(
            "Runtime rejected Workspace activation: {}",
            response.text().await?
        );
    }
    print_workspace(&response.json().await?);
    Ok(())
}

pub(crate) async fn remote_workspaces(home: &Path) -> Result<Vec<RuntimeWorkspace>> {
    let state = ensure_running(home).await?;
    authorized_get(&state, "/v1/workspaces").await
}

pub(crate) async fn activate_remote_workspace(
    home: &Path,
    id: uuid::Uuid,
) -> Result<RuntimeWorkspace> {
    let state = ensure_running(home).await?;
    let response = client()
        .post(format!(
            "http://{}/v1/workspaces/{id}/activate",
            state.address
        ))
        .header(TOKEN_HEADER, &state.token)
        .send()
        .await?;
    if !response.status().is_success() {
        bail!(
            "Runtime rejected Workspace activation: {}",
            response.text().await?
        );
    }
    Ok(response.json().await?)
}

pub(super) async fn remove_cli(home: &Path, id: uuid::Uuid, yes: bool) -> Result<()> {
    if !yes {
        bail!("Workspace removal requires --yes; files and Sessions are not deleted");
    }
    let state = ensure_running(home).await?;
    let response = client()
        .delete(format!("http://{}/v1/workspaces/{id}", state.address))
        .header(TOKEN_HEADER, &state.token)
        .send()
        .await?;
    if !response.status().is_success() {
        bail!(
            "Runtime rejected Workspace removal: {}",
            response.text().await?
        );
    }
    println!("removed\tworkspace={id}\tfiles=preserved\tsessions=preserved");
    Ok(())
}

fn print_workspace(workspace: &RuntimeWorkspace) {
    println!(
        "{}\t{}\t{}\taccess={:?}\tprofile={}\tskills={}\tmcp={}\t{}",
        workspace.id,
        if workspace.active {
            "active"
        } else {
            "registered"
        },
        workspace.name,
        workspace.access,
        workspace.provider_profile.as_deref().unwrap_or("-"),
        workspace.skills.len(),
        workspace.mcp_servers.len(),
        workspace.root.display()
    );
}

fn canonical_directory(path: &Path) -> Result<PathBuf> {
    let root = path
        .canonicalize()
        .with_context(|| format!("invalid Workspace root: {}", path.display()))?;
    if !root.is_dir() {
        bail!("Workspace root is not a directory: {}", root.display());
    }
    Ok(root)
}

fn normalize_name(name: Option<String>, root: &Path) -> Result<String> {
    let fallback = root
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("workspace");
    let name = name
        .unwrap_or_else(|| fallback.to_owned())
        .trim()
        .to_owned();
    if name.is_empty() {
        bail!("Workspace name must not be empty");
    }
    if name.chars().count() > MAX_WORKSPACE_NAME_CHARS {
        bail!("Workspace name exceeds {MAX_WORKSPACE_NAME_CHARS} characters");
    }
    Ok(name)
}

fn normalize_optional(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim().to_owned();
        (!value.is_empty()).then_some(value)
    })
}

fn normalize_names(values: Vec<String>, label: &str) -> Result<Vec<String>> {
    let mut result = Vec::new();
    for value in values {
        let value = value.trim().to_owned();
        if value.is_empty() {
            bail!("{label} name must not be empty");
        }
        if !result.contains(&value) {
            result.push(value);
        }
    }
    Ok(result)
}

fn mark_active(items: &mut [RuntimeWorkspace], active_id: Option<uuid::Uuid>) {
    for item in items {
        item.active = active_id == Some(item.id);
    }
}

fn persist(path: &Path, state: &PersistedWorkspaces) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_json_atomic(path, state)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("willdeep-{label}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn register_update_activate_and_persist_workspaces() {
        let home = temporary_root("workspace-store");
        let first = home.join("first");
        let second = home.join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let path = home.join("workspaces.json");
        let store = WorkspaceStore::open(path.clone()).unwrap();
        let (first_item, created) = store
            .register(RegisterWorkspace {
                root: first.clone(),
                name: Some("First".to_owned()),
                access: WorkspaceAccess::ReadOnly,
                provider_profile: Some("review".to_owned()),
                skills: vec!["reader".to_owned(), "reader".to_owned()],
                mcp_servers: vec!["docs".to_owned()],
            })
            .unwrap();
        assert!(created);
        assert!(first_item.active);
        assert_eq!(first_item.skills, vec!["reader"]);
        let (second_item, _) = store
            .register(RegisterWorkspace {
                root: second,
                name: None,
                access: WorkspaceAccess::WorkspaceWrite,
                provider_profile: None,
                skills: Vec::new(),
                mcp_servers: Vec::new(),
            })
            .unwrap();
        assert!(!second_item.active);
        assert!(store.activate(second_item.id).unwrap().unwrap().active);
        drop(store);

        let reopened = WorkspaceStore::open(path).unwrap();
        assert_eq!(reopened.active().unwrap().unwrap().id, second_item.id);
        assert_eq!(reopened.list().unwrap().len(), 2);
        reopened.remove(second_item.id).unwrap().unwrap();
        assert_eq!(reopened.active().unwrap().unwrap().id, first_item.id);
    }
}
