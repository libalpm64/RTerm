export const sessions = new Map();
export const activeKeywords = [];
export let semanticHlEnabled = true;
export const commandHistory = [];
export const MAX_COMMAND_HISTORY = 50;
export const MAX_HISTORY_ENTRY_LENGTH = 4096;
export let activeId = null;
export let fontSize = 13;
export let cursorStyle = 'block';
export let cursorBlink = true;
export let scrollback = 2000;
export const MAX_SCROLLBACK_LINES = 100000;

export let activeMenu = null;
export let currentView = 'terminal';
export let currentTerm = null;
export let currentFitAddon = null;

export const savedSessions = [];
export let savedSessionsLoaded = false;
export let savedSessionsDirty = false;

export let _loadedKeys = [];
export let _editingIndex = -1;
export let _sdResolve = null;
export let _sdPath = '~';
export let _sdFilename = '';
export let _sdMode = 'folder';

export let sftpSshId = null;
export let sftpInitialized = false;

export let localPath = '~';
export let localExpanded = '';
export let localHome = '';

export let keysTabId = 'keys-' + Date.now();
export let keysTabOpen = false;

export let draggedTabId = null;
export let _fitRAF = null;
export let connecting = false;

export function setActiveId(val) { activeId = val; }
export function setFontSize(val) { fontSize = val; }
export function setCursorStyle(val) { cursorStyle = val; }
export function setCursorBlink(val) { cursorBlink = val; }
export function normalizeScrollback(val) {
  const parsed = Number.parseInt(val, 10);
  // Bound xterm scrollback to a safe maximum
  if (!Number.isFinite(parsed) || parsed <= 0) return MAX_SCROLLBACK_LINES;
  return Math.min(parsed, MAX_SCROLLBACK_LINES);
}
export function setScrollback(val) { scrollback = normalizeScrollback(val); }
export function setSemanticHlEnabled(val) { semanticHlEnabled = val; }
export function setActiveMenu(val) { activeMenu = val; }
export function setCurrentView(val) { currentView = val; }
export function setCurrentTerm(val) { currentTerm = val; }
export function setCurrentFitAddon(val) { currentFitAddon = val; }
export function setSavedSessionsLoaded(val) { savedSessionsLoaded = val; }
export function setSavedSessionsDirty(val) { savedSessionsDirty = val; }
export function setLoadedKeys(val) { _loadedKeys = val; }
export function setEditingIndex(val) { _editingIndex = val; }
export function setSdResolve(val) { _sdResolve = val; }
export function setSdPath(val) { _sdPath = val; }
export function setSdFilename(val) { _sdFilename = val; }
export function setSdMode(val) { _sdMode = val; }
export function setSftpSshId(val) { sftpSshId = val; }
export function setSftpInitialized(val) { sftpInitialized = val; }
export function setLocalPath(val) { localPath = val; }
export function setLocalExpanded(val) { localExpanded = val; }
export function setLocalHome(val) { localHome = val; }
export function setKeysTabId(val) { keysTabId = val; }
export function setKeysTabOpen(val) { keysTabOpen = val; }
export function setDraggedTabId(val) { draggedTabId = val; }
export function setConnecting(val) { connecting = val; }
