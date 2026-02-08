use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputAction {
    None,
    Quit,
    Save,
    Toggle,
    ToggleFocus,
    Next,
    Prev,
    Backspace,
    Clear,
    Help,
    ShowPackageInfo,
    OpenVersionPicker,
    OpenEnv,
    OpenShell,
    ToggleBroken,
    ToggleInsecure,
    ToggleInstalled,
    ToggleSearchMode,
    ToggleDetails,
    EditLicenseFilter,
    EditPlatformFilter,
    PreviewDiff,
    UpdatePin,
    AddPin,
    TogglePresets,
    ToggleChanges,
    OpenColumns,
    RebuildIndex,
    Sync,
    Insert(char),
}

pub fn map_key(event: KeyEvent) -> InputAction {
    match event.code {
        KeyCode::Esc => InputAction::Quit,
        KeyCode::Char('q') if event.modifiers.contains(KeyModifiers::CONTROL) => InputAction::Quit,
        KeyCode::Char('s') if event.modifiers.contains(KeyModifiers::CONTROL) => InputAction::Save,
        KeyCode::Down => InputAction::Next,
        KeyCode::Up => InputAction::Prev,
        KeyCode::Char('?') => InputAction::Help,
        KeyCode::Char('i') if event.modifiers.contains(KeyModifiers::CONTROL) => {
            InputAction::ShowPackageInfo
        }
        KeyCode::Char('p') if event.modifiers.contains(KeyModifiers::CONTROL) => {
            InputAction::ShowPackageInfo
        }
        KeyCode::Char('v') if event.modifiers.contains(KeyModifiers::CONTROL) => {
            InputAction::OpenVersionPicker
        }
        KeyCode::Char('E') => InputAction::OpenEnv,
        KeyCode::Char('H') => InputAction::OpenShell,
        KeyCode::Char('B') => InputAction::ToggleBroken,
        KeyCode::Char('I') => InputAction::ToggleInsecure,
        KeyCode::Char('V') => InputAction::ToggleInstalled,
        KeyCode::Char('S') => InputAction::ToggleSearchMode,
        KeyCode::Char('K') => InputAction::ToggleDetails,
        KeyCode::Char('L') => InputAction::EditLicenseFilter,
        KeyCode::Char('O') => InputAction::EditPlatformFilter,
        KeyCode::Char('D') => InputAction::PreviewDiff,
        KeyCode::Char('U') => InputAction::UpdatePin,
        KeyCode::Char('n') if event.modifiers.contains(KeyModifiers::CONTROL) => {
            InputAction::AddPin
        }
        KeyCode::Char('T') => InputAction::TogglePresets,
        KeyCode::Char('C') => InputAction::ToggleChanges,
        KeyCode::Char('M') => InputAction::OpenColumns,
        KeyCode::Char('R') => InputAction::RebuildIndex,
        KeyCode::Char('Y') => InputAction::Sync,
        KeyCode::Enter => InputAction::Toggle,
        KeyCode::Char(' ') => InputAction::Toggle,
        KeyCode::Tab => InputAction::ToggleFocus,
        KeyCode::Backspace => InputAction::Backspace,
        KeyCode::Char('u') if event.modifiers.contains(KeyModifiers::CONTROL) => InputAction::Clear,
        KeyCode::Char(ch) if event.modifiers.contains(KeyModifiers::CONTROL) => InputAction::None,
        KeyCode::Char(ch) => InputAction::Insert(ch),
        _ => InputAction::None,
    }
}
