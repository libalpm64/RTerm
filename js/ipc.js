import { sessions, activeKeywords, semanticHlEnabled, activeId } from './state.js';
import { applyHighlighting } from './highlighting.js';

if (typeof window.ipc === 'undefined') {
  try {
    Object.defineProperty(window, 'ipc', {
      value: { postMessage: function() {} },
      writable: true,
      configurable: true
    });
  } catch (e) {}
}

window.__rterm_pending = {};
window.__rterm_rid = 0;
const MAX_PENDING_IPC = 256;
let _ipcPendingCount = 0;
let _sftpDownloadSeq = 0;
const _terminalWriteQueue = new Map();
let _terminalFlushQueued = false;
window.__rterm_resp = function (data) {
  const rid = data && data._rid;
  const pending = rid != null ? window.__rterm_pending[rid] : null;
  if (pending) {
    clearTimeout(pending.timer);
    _ipcPendingCount = Math.max(0, _ipcPendingCount - 1);
    pending.resolve(data);
    delete window.__rterm_pending[rid];
  }
};

export function _ipc(method, args) {
  if (_ipcPendingCount >= MAX_PENDING_IPC) {
    return Promise.resolve({ success: false, error: 'IPC request queue is busy' });
  }
  return new Promise((resolve) => {
    const rid = ++window.__rterm_rid;
    const pending = {
      resolve,
      timer: setTimeout(() => {
        if (window.__rterm_pending[rid] !== pending) return;
        delete window.__rterm_pending[rid];
        _ipcPendingCount = Math.max(0, _ipcPendingCount - 1);
        resolve({ success: false, error: 'IPC request timed out' });
      }, 30000),
    };
    window.__rterm_pending[rid] = pending;
    _ipcPendingCount += 1;
    window.ipc.postMessage(JSON.stringify({ _rid: rid, method, args: args || {} }));
  });
}

function flushTerminalWrites() {
  _terminalFlushQueued = false;
  for (const [id, chunks] of _terminalWriteQueue) {
    _terminalWriteQueue.delete(id);
    const data = chunks.length === 1 ? chunks[0] : chunks.join('');
    for (const [, sess] of sessions) {
      if (sess.sshId === id && sess.term) {
        sess.term.write(applyHighlighting(data));
        break;
      }
    }
  }
}

window.__rterm_onData = function (id, data) {
  if (data === 'EOF') return;
  let chunks = _terminalWriteQueue.get(id);
  if (!chunks) {
    chunks = [];
    _terminalWriteQueue.set(id, chunks);
  }
  chunks.push(data);
  if (!_terminalFlushQueued) {
    _terminalFlushQueued = true;
    queueMicrotask(flushTerminalWrites);
  }
};

export const invoke = window.__TAURI__?.core?.invoke ?? null;

export function setupRtermApi() {
  window.rterm = {
    sshConnect: function (config) { return _ipc("ssh_connect", config); },
    sshShell: function (id) { return _ipc("ssh_shell", { id }); },
    sshWrite: function (id, data) { window.ipc.postMessage(JSON.stringify({ method: "ssh_write", args: { id, data } })); return Promise.resolve(); },
    sshResize: function (id, cols, rows) { return _ipc("ssh_resize", { id, cols, rows }); },
    sshDisconnect: function (id) { return _ipc("ssh_disconnect", { id }); },
    saveSessions: function (sessions, password, keys) { return _ipc("save_sessions", { sessions: sessions || [], password: password || '', keys: keys || [] }); },
    loadSessions: function (password) { return _ipc("load_sessions", { password: password || '' }); },
    saveSetting: function (key, value) { return _ipc("save_setting", { key, value }); },
    loadSetting: function (key) { return _ipc("load_setting", { key }); },
    vaultExists: function () { return _ipc("vault_exists", {}); },
    deleteVault: function () { return _ipc("delete_vault", {}); },
    importVaultKey: function (name, privateKey, publicKey, keyType, password) { return _ipc("import_vault_key", { name, private_key: privateKey, public_key: publicKey, key_type: keyType, password }); },
    deleteVaultKey: function (name, password) { return _ipc("delete_vault_key", { name, password }); },
    sshExec: function (id, cmd) { return _ipc("ssh_exec", { id, command: cmd }); },
    sftpOpen: function (id) { return _ipc("sftp_open", { id }); },
    sftpList: function (id, path) { return _ipc("sftp_list", { id, path: path || '.' }); },
    sftpOpenFile: function (id, path) { return _ipc("sftp_open_file", { id, path }); },
    sftpRead: function (handle, size) { return _ipc("sftp_read", { handle, size: size || 65536 }); },
    sftpCloseFile: function (handle) { return _ipc("sftp_close_file", { handle }); },
    sftpDownload: function (id, path, filename, save_path, transferId) {
      const transfer_id = transferId || `download-${Date.now().toString(36)}-${++_sftpDownloadSeq}`;
      window.__rterm_transferProgress?.(transfer_id, `Queued ${filename || 'download'}`, 0);
      return _ipc("sftp_download", { id, path, filename, save_path: save_path || '', transfer_id }).then((result) => {
        if (!result?.success) {
          window.__rterm_transferProgress?.(transfer_id, result?.error || 'Download failed', 100, { error: true });
        }
        return { ...result, transfer_id };
      });
    },
    sftpRename: function (id, oldPath, newPath) { return _ipc("sftp_rename", { id, old_path: oldPath, new_path: newPath }); },
    localExec: function (cmd, cols) { return _ipc("local_exec", { command: cmd, cols: cols || 120 }); },
    localList: function (path) { return _ipc("local_list", { path: path || '.' }); },
    localDelete: function (path, dir) { return _ipc("local_delete", { path, dir: dir || false }); },
    localMove: function (fromPath, toPath) { return _ipc("local_move", { from_path: fromPath, to_path: toPath }); },
    generateKey: function (name, type, passphrase) { return _ipc("generate_key", { name, type, passphrase: passphrase || '' }); },
    listKeys: function () { return _ipc("list_keys", {}); },
    deleteKey: function (path) { return _ipc("delete_key", { path }); },
    getEnv: function (key) { return _ipc("get_env", { key }); },
    webviewMemory: function () { return _ipc("webview_memory", {}); },
    openDevtools: function () { return _ipc("open_devtools", {}); },
    sftpUpload: function (id, localPath, remotePath) { return _ipc("sftp_upload", { id, local_path: localPath, remote_path: remotePath }); },
    telnetConnect: function (config) { return _ipc("telnet_connect", config); },
    serialConnect: function (config) { return _ipc("serial_connect", config); },
  };
}
