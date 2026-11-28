//! Shared Vault modals (`Env`, `Skills`, `Tools`, `References`, `Models`, `Servers`) and project sync UI.

use super::*;

/// Target for shared vault modals (`Env`, `Skills`, `Tools`, `References`, or the Add/Edit form).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum VaultTarget {
    /// Opened from the Add/Edit project modal; `Enter` confirms selection back to `add_*`.
    AddForm,
    /// Opened from Manager on project `pi`; `Enter` writes checked items to `<project>`.
    Project(usize),
    /// Opened from Manager with no projects registered; manages the shared vault only.
    VaultOnly,
}

/// Inline form inside `VaultModal` (`Env`) for registering or editing a shared env variable.
pub(crate) struct EnvEditor {
    pub(crate) key: LineInput,
    pub(crate) value: LineInput,
    /// Active input field: `0` = key, `1` = value.
    pub(crate) field: usize,
    /// Original key when editing an existing entry; `None` when creating a new one.
    pub(crate) editing_key: Option<String>,
    pub(crate) err: String,
}

impl EnvEditor {
    /// An empty form for a new variable.
    pub(crate) fn blank() -> Self {
        Self {
            key: LineInput::new(),
            value: LineInput::new(),
            field: 0,
            editing_key: None,
            err: String::new(),
        }
    }
}

/// Inline form inside `VaultModal` (`Skill`) for registering or editing a shared skill.
pub(crate) struct SkillEditor {
    pub(crate) name: LineInput,
    pub(crate) desc: LineInput,
    pub(crate) body: LineInput,
    /// Active input field: `0` = name, `1` = description, `2` = source path or instructions.
    pub(crate) field: usize,
    pub(crate) editing_name: Option<String>,
    /// Preserves multi-line TOML instructions when the body line wasn't modified in TUI.
    pub(crate) initial_body_preview: String,
    pub(crate) preserved_multiline: Option<String>,
    pub(crate) err: String,
}

impl SkillEditor {
    pub(crate) fn blank() -> Self {
        Self {
            name: LineInput::new(),
            desc: LineInput::new(),
            body: LineInput::new(),
            field: 0,
            editing_name: None,
            initial_body_preview: String::new(),
            preserved_multiline: None,
            err: String::new(),
        }
    }
}

/// Inline form inside `VaultModal` (`Tool`) for registering or editing a shared MCP or CLI tool.
pub(crate) struct ToolEditor {
    pub(crate) kind: ToolKind,
    pub(crate) name: LineInput,
    pub(crate) command: LineInput,
    pub(crate) desc: LineInput,
    /// Active input field: `0` = kind toggle, `1` = name, `2` = cmd/url, `3` = description.
    pub(crate) field: usize,
    pub(crate) editing_name: Option<String>,
    pub(crate) preserved_env: BTreeMap<String, String>,
    pub(crate) err: String,
}

impl ToolEditor {
    pub(crate) fn blank() -> Self {
        Self {
            kind: ToolKind::Mcp,
            name: LineInput::new(),
            command: LineInput::new(),
            desc: LineInput::new(),
            field: 0,
            editing_name: None,
            preserved_env: BTreeMap::new(),
            err: String::new(),
        }
    }
}

/// Inline form inside `VaultModal` (`Ref`) for registering or editing a shared reference link.
pub(crate) struct RefEditor {
    pub(crate) name: LineInput,
    pub(crate) url: LineInput,
    pub(crate) desc: LineInput,
    /// Active input field: `0` = name (optional; derived from URL if blank), `1` = url, `2` = desc.
    pub(crate) field: usize,
    pub(crate) editing_name: Option<String>,
    pub(crate) err: String,
}

impl RefEditor {
    pub(crate) fn blank() -> Self {
        Self {
            name: LineInput::new(),
            url: LineInput::new(),
            desc: LineInput::new(),
            // Start on URL so the user can paste a link right away; Name auto-fills if empty.
            field: 1,
            editing_name: None,
            err: String::new(),
        }
    }
}

/// Inline form inside `VaultModal` (`Model`) for a shared LLM model.
pub(crate) struct ModelEditor {
    pub(crate) provider: Provider,
    pub(crate) name: LineInput,
    pub(crate) model: LineInput,
    pub(crate) endpoint: LineInput,
    pub(crate) token: LineInput,
    /// `0` = provider toggle, `1` name, `2` model, `3` endpoint, `4` token.
    pub(crate) field: usize,
    pub(crate) editing_name: Option<String>,
    pub(crate) err: String,
}

impl ModelEditor {
    pub(crate) fn blank() -> Self {
        Self {
            provider: Provider::default(),
            name: LineInput::new(),
            model: LineInput::new(),
            endpoint: LineInput::new(),
            token: LineInput::new(),
            field: 1,
            editing_name: None,
            err: String::new(),
        }
    }
    pub(crate) fn input(&mut self) -> Option<&mut LineInput> {
        match self.field {
            1 => Some(&mut self.name),
            2 => Some(&mut self.model),
            3 => Some(&mut self.endpoint),
            4 => Some(&mut self.token),
            _ => None,
        }
    }
}

/// Kind-specific state carried inside [`VaultModal`].
pub(crate) enum VaultKindState {
    Env {
        /// Existing keys/values parsed from the target project's `.env` file.
        project_dotenv: BTreeMap<String, String>,
        reveal: bool,
        editor: Option<EnvEditor>,
    },
    Skill {
        project_state: ProjectSkills,
        /// Skills whose same-named folders in the project differ from the vault
        /// version: the sync waits for y (overwrite), n (keep them) or Esc.
        confirm_overwrite: Option<Vec<String>>,
        editor: Option<SkillEditor>,
    },
    Tool {
        project_state: ProjectTools,
        editor: Option<ToolEditor>,
    },
    Ref {
        project_state: ProjectReferences,
        editor: Option<RefEditor>,
    },
    Model {
        /// The target project's `.env`, where models are written.
        project_dotenv: BTreeMap<String, String>,
        reveal: bool,
        editor: Option<ModelEditor>,
    },
    /// Registered servers: the env editor's key is the name, value the address.
    Server { editor: Option<EnvEditor> },
}

/// Unified Shared Vault & project-sync popup (`Env`, `Skills`, `Tools`, `References`).
/// `sel` runs one past the last entry: that row is `"+ New <noun>..."`.
pub(crate) struct VaultModal {
    pub(crate) target: VaultTarget,
    pub(crate) sel: usize,
    pub(crate) top: std::cell::Cell<usize>,
    pub(crate) picked: BTreeSet<String>,
    pub(crate) confirm_delete: Option<String>,
    pub(crate) kind: VaultKindState,
}

impl VaultModal {
    pub(crate) fn new(target: VaultTarget, picked: BTreeSet<String>, kind: VaultKindState) -> Self {
        Self {
            target,
            sel: 0,
            top: Default::default(),
            picked,
            confirm_delete: None,
            kind,
        }
    }

    pub(crate) fn is_env(&self) -> bool {
        matches!(self.kind, VaultKindState::Env { .. })
    }

    pub(crate) fn is_skill(&self) -> bool {
        matches!(self.kind, VaultKindState::Skill { .. })
    }

    pub(crate) fn is_tool(&self) -> bool {
        matches!(self.kind, VaultKindState::Tool { .. })
    }

    pub(crate) fn is_ref(&self) -> bool {
        matches!(self.kind, VaultKindState::Ref { .. })
    }

    pub(crate) fn is_model(&self) -> bool {
        matches!(self.kind, VaultKindState::Model { .. })
    }

    pub(crate) fn is_server(&self) -> bool {
        matches!(self.kind, VaultKindState::Server { .. })
    }

    pub(crate) fn has_editor(&self) -> bool {
        match &self.kind {
            VaultKindState::Env { editor, .. } => editor.is_some(),
            VaultKindState::Skill { editor, .. } => editor.is_some(),
            VaultKindState::Tool { editor, .. } => editor.is_some(),
            VaultKindState::Ref { editor, .. } => editor.is_some(),
            VaultKindState::Model { editor, .. } => editor.is_some(),
            VaultKindState::Server { editor } => editor.is_some(),
        }
    }

    pub(crate) fn wants_text_input(&self) -> bool {
        match &self.kind {
            VaultKindState::Env { editor, .. } => editor.is_some(),
            VaultKindState::Skill { editor, .. } => editor.is_some(),
            VaultKindState::Tool { editor, .. } => editor.as_ref().is_some_and(|e| e.field != 0),
            VaultKindState::Ref { editor, .. } => editor.is_some(),
            VaultKindState::Model { editor, .. } => editor.as_ref().is_some_and(|e| e.field != 0),
            VaultKindState::Server { editor } => editor.is_some(),
        }
    }

    pub(crate) fn open_blank_editor(&mut self) {
        match &mut self.kind {
            VaultKindState::Env { editor, .. } => *editor = Some(EnvEditor::blank()),
            VaultKindState::Skill { editor, .. } => *editor = Some(SkillEditor::blank()),
            VaultKindState::Tool { editor, .. } => *editor = Some(ToolEditor::blank()),
            VaultKindState::Ref { editor, .. } => *editor = Some(RefEditor::blank()),
            VaultKindState::Model { editor, .. } => *editor = Some(ModelEditor::blank()),
            VaultKindState::Server { editor } => *editor = Some(EnvEditor::blank()),
        }
    }

    pub(crate) fn close_editor(&mut self) {
        match &mut self.kind {
            VaultKindState::Env { editor, .. } => *editor = None,
            VaultKindState::Skill { editor, .. } => *editor = None,
            VaultKindState::Tool { editor, .. } => *editor = None,
            VaultKindState::Ref { editor, .. } => *editor = None,
            VaultKindState::Model { editor, .. } => *editor = None,
            VaultKindState::Server { editor } => *editor = None,
        }
    }

    pub(crate) fn paste_into_editor(&mut self, data: &str) -> bool {
        let chars = data.chars().filter(|&c| c != '\r' && c != '\n');
        match &mut self.kind {
            VaultKindState::Env {
                editor: Some(ed), ..
            }
            | VaultKindState::Server { editor: Some(ed) } => {
                let input = if ed.field == 0 {
                    &mut ed.key
                } else {
                    &mut ed.value
                };
                for ch in chars {
                    input.insert(ch);
                }
                ed.err.clear();
                true
            }
            VaultKindState::Skill {
                editor: Some(ed), ..
            } => {
                let input = match ed.field {
                    0 => &mut ed.name,
                    1 => &mut ed.desc,
                    _ => &mut ed.body,
                };
                for ch in chars {
                    input.insert(ch);
                }
                ed.err.clear();
                true
            }
            VaultKindState::Tool {
                editor: Some(ed), ..
            } => {
                if ed.field != 0 {
                    let input = match ed.field {
                        1 => &mut ed.name,
                        2 => &mut ed.command,
                        _ => &mut ed.desc,
                    };
                    for ch in chars {
                        input.insert(ch);
                    }
                    ed.err.clear();
                }
                true
            }
            VaultKindState::Ref {
                editor: Some(ed), ..
            } => {
                let input = match ed.field {
                    0 => &mut ed.name,
                    1 => &mut ed.url,
                    _ => &mut ed.desc,
                };
                for ch in chars {
                    input.insert(ch);
                }
                ed.err.clear();
                true
            }
            VaultKindState::Model {
                editor: Some(ed), ..
            } => {
                if let Some(input) = ed.input() {
                    for ch in chars {
                        input.insert(ch);
                    }
                    ed.err.clear();
                }
                true
            }
            _ => false,
        }
    }

    pub(crate) fn editor_rows(&self) -> u16 {
        match &self.kind {
            VaultKindState::Env { .. } => 5,
            VaultKindState::Skill { .. } => 6,
            VaultKindState::Tool { .. } => 7,
            VaultKindState::Ref { .. } => 6,
            VaultKindState::Model { .. } => 8,
            VaultKindState::Server { .. } => 5,
        }
    }

    pub(crate) fn close_hotkey(&self) -> char {
        match &self.kind {
            VaultKindState::Env { .. } => 'E',
            VaultKindState::Skill { .. } => 'K',
            VaultKindState::Tool { .. } => 'T',
            VaultKindState::Ref { .. } => 'R',
            VaultKindState::Model { .. } => 'O',
            VaultKindState::Server { .. } => 'V',
        }
    }

    pub(crate) fn noun(&self) -> &'static str {
        match &self.kind {
            VaultKindState::Env { .. } => "variable",
            VaultKindState::Skill { .. } => "skill",
            VaultKindState::Tool { .. } => "tool",
            VaultKindState::Ref { .. } => "reference",
            VaultKindState::Model { .. } => "model",
            VaultKindState::Server { .. } => "server",
        }
    }

    pub(crate) fn delete_kind(&self) -> &'static str {
        match &self.kind {
            VaultKindState::Env { .. } => "env",
            VaultKindState::Skill { .. } => "skill",
            VaultKindState::Tool { .. } => "tool",
            VaultKindState::Ref { .. } => "reference",
            VaultKindState::Model { .. } => "model",
            VaultKindState::Server { .. } => "server",
        }
    }

    pub(crate) fn footer_verb(&self) -> &'static str {
        match &self.kind {
            VaultKindState::Env { .. } => "write .env",
            VaultKindState::Skill { .. } => "sync skills",
            VaultKindState::Tool { .. } => "sync tools",
            VaultKindState::Ref { .. } => "sync refs",
            VaultKindState::Model { .. } => "write .env",
            VaultKindState::Server { .. } => "",
        }
    }

    pub(crate) fn empty_hint(&self) -> &'static str {
        match &self.kind {
            VaultKindState::Env { .. } => {
                "  No shared env vars registered yet. Press a to create one."
            }
            VaultKindState::Skill { .. } => {
                "  No shared skills registered yet. Press a to add one."
            }
            VaultKindState::Tool { .. } => {
                "  No shared tools registered yet. Press a to add an MCP or CLI tool."
            }
            VaultKindState::Ref { .. } => {
                "  No shared references registered yet. Press a to add a GitHub link."
            }
            VaultKindState::Model { .. } => {
                "  No LLM models registered yet. Press a to add a model, endpoint and token."
            }
            VaultKindState::Server { .. } => {
                "  No servers registered yet. Press a to add a name and user@host."
            }
        }
    }

    pub(crate) fn editor_footer_hint(&self) -> &'static str {
        match &self.kind {
            VaultKindState::Env { .. } => "Saved entries stay in your shared vault for any project",
            VaultKindState::Skill { .. } => {
                "Applies to .agents/skills + .claude/skills (agy, claude, muse, codex)"
            }
            VaultKindState::Tool { .. } => {
                "MCP syncs .mcp.json + .agents/mcp_config.json + .codex/config.toml; CLI writes .squad/TOOLS.md"
            }
            VaultKindState::Ref { .. } => {
                "Syncs CLAUDE.md + AGENTS.md (+ GEMINI.md if present); blank Name uses URL slug"
            }
            VaultKindState::Model { .. } => {
                "Written to .env as the provider's variables; sessions started there get them"
            }
            VaultKindState::Server { .. } => {
                "Address as ssh takes it: user@host or an ssh alias; pick it in the Add form"
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn project_dotenv(&self) -> Option<&BTreeMap<String, String>> {
        match &self.kind {
            VaultKindState::Env { project_dotenv, .. } => Some(project_dotenv),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn skill_overwrite(&self) -> Option<&Vec<String>> {
        match &self.kind {
            VaultKindState::Skill {
                confirm_overwrite, ..
            } => confirm_overwrite.as_ref(),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn tool_kind(&self) -> Option<ToolKind> {
        match &self.kind {
            VaultKindState::Tool {
                editor: Some(ed), ..
            } => Some(ed.kind),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn ref_field(&self) -> Option<usize> {
        match &self.kind {
            VaultKindState::Ref {
                editor: Some(ed), ..
            } => Some(ed.field),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn ref_state(&self) -> Option<&ProjectReferences> {
        match &self.kind {
            VaultKindState::Ref { project_state, .. } => Some(project_state),
            _ => None,
        }
    }
}

mod actions;
mod draw;
mod save;
