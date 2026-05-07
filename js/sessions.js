import {
  sessions, activeId, savedSessions, savedSessionsLoaded, savedSessionsDirty,
  _editingIndex,
  setSavedSessionsLoaded, setSavedSessionsDirty, setEditingIndex, setActiveId
} from './state.js';
import { newTerminal, createTerminalInstance, renderTabs, selectTab } from './terminal.js';

export function renderSavedSessions() {
  if (!savedSessionsDirty) return;
  setSavedSessionsDirty(false);

  const list = document.getElementById('session-list');
  if (savedSessions.length === 0) {
    list.innerHTML = '<div class="sempty">No saved sessions</div>';
    return;
  }

  list.innerHTML = savedSessions.map((s, i) => `
    <div class="session-item" data-index="${i}">
      <span class="sicon">
        ${s.type === 'local' ? '<svg viewBox="0 0 24 24" fill="none" stroke="var(--green)" stroke-width="1.5"><rect x="2" y="3" width="20" height="14" rx="2" ry="2"/><line x1="8" y1="21" x2="16" y2="21"/><line x1="12" y1="17" x2="12" y2="21"/></svg>' :
          s.type === 'ssh' ? '<svg viewBox="0 0 24 24" fill="none" stroke="var(--blue)" stroke-width="1.5"><path d="M21 2l-2 2m-7.61 7.61a5.5 5.5 0 11-7.778 7.778 5.5 5.5 0 017.777-7.777zm0 0L15.5 7.5m0 0l3 3L22 7l-3-3m-3.5 3.5L19 4"/></svg>' :
            '<svg viewBox="0 0 24 24" fill="none" stroke="var(--purple)" stroke-width="1.5"><path d="M18 8h1a4 4 0 010 8h-1M2 8h16v9a4 4 0 01-4 4H6a4 4 0 01-4-4V8zM6 1v3M10 1v3M14 1v3"/></svg>'}
      </span>
      <div class="sinfo">
        <div class="sname">${s.name}</div>
        <div class="shost">${s.host || s.type}</div>
      </div>
      <div class="sactions">
        <span class="saction" data-action="edit" title="Edit">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M11 4H4a2 2 0 00-2 2v14a2 2 0 002 2h14a2 2 0 002-2v-7"/><path d="M18.5 2.5a2.121 2.121 0 013 3L12 15l-4 1 1-4 9.5-9.5z"/></svg>
        </span>
        <span class="saction" data-action="delete" title="Delete">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polyline points="3 6 5 6 21 6"/><path d="M19 6v14a2 2 0 01-2 2H7a2 2 0 01-2-2V6m3 0V4a2 2 0 012-2h4a2 2 0 012 2v2"/></svg>
        </span>
      </div>
    </div>
  `).join('');

  list.querySelectorAll('.session-item').forEach(item => {
    item.addEventListener('click', (e) => {
      if (e.target.closest('.saction')) {
        const action = e.target.closest('.saction').dataset.action;
        const idx = parseInt(item.dataset.index);
        if (action === 'edit') editSession(idx);
        else if (action === 'delete') deleteSession(idx);
      } else {
        connectSession(parseInt(item.dataset.index));
      }
    });
  });
}

export function addSession(type, data, save = true) {
  const session = {
    type,
    name: data.name || `${type} Session`,
    host: data.host || '',
    port: data.port || 22,
    user: data.user || '',
    ...data
  };
  savedSessions.push(session);
  setSavedSessionsDirty(true);
  if (save) renderSavedSessions();
}

export function editSession(idx) {
  const s = savedSessions[idx];

  import('./ui.js').then(mod => {
    mod.closeMenusInternal();
    mod.showModalInternal('connect');
    // Override title/button after showModalInternal
    document.getElementById('modal-title').textContent = 'Edit Session';
    document.getElementById('modal-connect-btn').textContent = 'Save';
  });

  setEditingIndex(idx);

  document.querySelectorAll('#modal-connect .modal-tab').forEach(t => t.classList.remove('active'));
  document.querySelector(`#modal-connect .modal-tab[data-type="${s.type}"]`).classList.add('active');

  document.getElementById('local-fields').style.display = 'none';
  document.getElementById('ssh-fields').style.display = 'none';
  document.getElementById('telnet-fields').style.display = 'none';
  document.getElementById('serial-fields').style.display = 'none';

  if (s.type === 'ssh') {
    document.getElementById('ssh-fields').style.display = '';
    document.getElementById('ssh-name').value = s.name || '';
    document.getElementById('ssh-host').value = s.host || '';
    document.getElementById('ssh-port').value = s.port || 22;
    document.getElementById('ssh-user').value = s.user || '';
    document.getElementById('ssh-pass').value = s.password || '';
    const useKey = s.key_path || s.key_name;
    document.getElementById('ssh-auth-method').value = useKey ? 'key' : 'password';
    document.getElementById('ssh-password-fields').style.display = useKey ? 'none' : '';

    if (useKey) {
      const sel = document.getElementById('ssh-key-select');
      sel.innerHTML = '<option value="">Select a key...</option>';
      const vaultKeys = window.__rterm_vault_keys || [];
      vaultKeys.forEach(k => {
        const opt = document.createElement('option');
        opt.value = k.name;
        opt.textContent = k.name + ' (vault)';
        opt.style.color = 'var(--accent)';
        sel.appendChild(opt);
      });
      if (window.rterm) {
        window.rterm.listKeys().then(keys => {
          if (keys.success && keys.result) {
            keys.result.forEach(k => {
              const opt = document.createElement('option');
              opt.value = k.path;
              opt.textContent = k.name;
              sel.appendChild(opt);
            });
            const keyVal = s.key_name || s.key_path;
            if (keyVal) sel.value = keyVal;
          }
        });
      }
      const keyVal = s.key_name || s.key_path;
      if (keyVal) sel.value = keyVal;
      document.getElementById('ssh-key-fields').style.display = '';
    } else {
      document.getElementById('ssh-key-fields').style.display = 'none';
    }
  }
}

export function deleteSession(idx) {
  savedSessions.splice(idx, 1);
  setSavedSessionsDirty(true);
  if (window.__rterm_vault_pass) {
    window.rterm.saveSessions(savedSessions.map(s => ({
      host: s.host, port: s.port,
      user: s.user, password: s.password,
      key_path: s.key_path, key_name: s.key_name,
      name: s.name
    })), window.__rterm_vault_pass);
  }
  renderSavedSessions();
}

export function connectSession(idx) {
  const s = savedSessions[idx];
  if (!s || !s.host) return;

  const id = 'session-' + Date.now();
  sessions.set(id, { name: s.name, type: 'ssh', term: null, fitAddon: null, sshId: null });
  setActiveId(id);

  renderTabs();
  createTerminalInstance(id, async (term, sess) => {
    if (window.rterm) {
      const config = {
        host: s.host, port: s.port || 22,
        user: s.user, password: s.password, key_path: s.key_path,
        key_name: s.key_name,
        vault_pass: s.key_name ? (window.__rterm_vault_pass || s.vault_pass) : null
      };
      try {
        const result = await window.rterm.sshConnect(config);
        if (!result.success) {
          term.write('\x1b[1;31mFailed to connect: ' + (result.error || 'unknown error') + '\x1b[0m\r\n');
          return;
        }
        sess.sshId = result.id;
        const shellResult = await window.rterm.sshShell(result.id);
        if (!shellResult.success) {
          term.write('\x1b[1;31mShell failed: ' + (shellResult.error || 'unknown error') + '\x1b[0m\r\n');
          return;
        }
        setTimeout(() => {
          if (sess.fitAddon) {
            sess.fitAddon.fit();
            const dims = sess.fitAddon.proposeDimensions();
            if (dims) window.rterm?.sshResize(result.id, dims.cols, dims.rows);
          }
          window.rterm?.sshWrite(result.id, 'clear\r');
          term.focus();
        }, 200);
        window.dispatchEvent(new CustomEvent('rterm:ssh-connected', {
          detail: { sessionId: id, sshId: result.id }
        }));
      } catch (e) {
        term.write('\x1b[1;31mError: ' + e + '\x1b[0m\r\n');
      }
    }
  });
}

export async function initSessions() {
  setTimeout(() => { if (sessions.size === 0) newTerminal(); }, 500);
}

export async function loadSavedSessions() {
  if (window.rterm?.loadSessions) {
    try {
      const result = await window.rterm.loadSessions();
      if (result.success && result.result) return result.result;
    } catch (e) { }
  }
  return [];
}

export async function saveSessionToBackend(name, type, config) {
  if (window.rterm?.saveSessions && window.__rterm_vault_pass) {
    try {
      await window.rterm.saveSessions([{
        host: config.host, port: config.port,
        user: config.user, password: config.password,
        key_path: config.key_path, name: name
      }], window.__rterm_vault_pass);
    } catch (e) { }
  }
}

function loadSavedSessionsLocal() {
  try {
    const saved = JSON.parse(localStorage.getItem('rterm_saved_sessions') || '[]');
    return saved;
  } catch (e) { return []; }
}

export async function initSavedSessions() {
  if (savedSessionsLoaded) return;
  setSavedSessionsLoaded(true);
  const loaded = await loadSavedSessions();
  loaded.forEach(s => savedSessions.push({ ...s, type: 'ssh', name: s.name || s.user + '@' + s.host }));
  setSavedSessionsDirty(true);
  if (typeof renderSavedSessions === 'function') renderSavedSessions();
}
