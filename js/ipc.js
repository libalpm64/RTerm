import { sessions, activeKeywords, semanticHlEnabled, activeId } from './state.js';

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
window.__rterm_resp = function (data) {
  const rid = data && data._rid;
  if (rid != null && window.__rterm_pending[rid]) {
    window.__rterm_pending[rid](data);
    delete window.__rterm_pending[rid];
  }
};

export function _ipc(method, args) {
  return new Promise((resolve) => {
    const rid = ++window.__rterm_rid;
    window.__rterm_pending[rid] = resolve;
    window.ipc.postMessage(JSON.stringify({ _rid: rid, method, args: args || {} }));
  });
}

window.__rterm_onData = function (id, data) {
  for (const [key, sess] of sessions) {
    if (sess.sshId === id && sess.term && data !== 'EOF') {
      import('./highlighting.js').then(mod => {
        const highlighted = mod.applyHighlighting(data);
        sess.term.write(highlighted);
      });
      break;
    }
  }
};

export const invoke = window.__TAURI__?.core?.invoke ?? null;

export function setupRtermApi() {
  window.rterm = {
    sshConnect: function (config) { return _ipc("ssh_connect", config); },
    sshShell: function (id) { return _ipc("ssh_shell", { id }); },
    sshWrite: function (id, data) { window.ipc.postMessage(JSON.stringify({ method: "ssh_write", args: { id, data } })); return Promise.resolve(); },
    sshResize: function (id, cols, rows) { return _ipc("ssh_resize", { id, cols, rows }); },
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
    sftpDownload: function (id, path, filename, save_path) { return _ipc("sftp_download", { id, path, filename, save_path: save_path || '' }); },
    sftpRename: function (id, oldPath, newPath) { return _ipc("sftp_rename", { id, old_path: oldPath, new_path: newPath }); },
    localExec: function (cmd, cols) { return _ipc("local_exec", { command: cmd, cols: cols || 120 }); },
    localList: function (path) { return _ipc("local_list", { path: path || '.' }); },
    localDelete: function (path, dir) { return _ipc("local_delete", { path, dir: dir || false }); },
    localMove: function (fromPath, toPath) { return _ipc("local_move", { from_path: fromPath, to_path: toPath }); },
    generateKey: function (name, type, passphrase) { return _ipc("generate_key", { name, type, passphrase: passphrase || '' }); },
    listKeys: function () { return _ipc("list_keys", {}); },
    deleteKey: function (path) { return _ipc("delete_key", { path }); },
    getEnv: function (key) { return _ipc("get_env", { key }); },
    sftpUpload: function (id, localPath, remotePath) { return _ipc("sftp_upload", { id, local_path: localPath, remote_path: remotePath }); },
    telnetConnect: function (config) { return _ipc("telnet_connect", config); },
    serialConnect: function (config) { return _ipc("serial_connect", config); },
  };
}
