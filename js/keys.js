import { _loadedKeys, setLoadedKeys } from './state.js';

export async function loadKeys() {
  if (!window.rterm) return;
  try {
    const list = document.getElementById('keys-list');
    if (!list) return;
    list.innerHTML = '';
    const allKeys = [];

    // Vault keys
    const vaultKeys = window.__rterm_vault_keys || [];
    vaultKeys.forEach(k => {
      allKeys.push({ ...k, type: k.key_type || 'ed25519', path: '(vault)', _vault: true });
    });

    // Filesystem keys
    const result = await window.rterm.listKeys();
    if (result.success && result.result) {
      result.result.forEach(k => allKeys.push({ ...k, _vault: false }));
    }

    setLoadedKeys(allKeys);
    allKeys.forEach((k, i) => {
      const d = document.createElement('div');
      d.className = 'key-item' + (i === 0 ? ' selected' : '');
      d.dataset.key = k.name;
      d.dataset.path = k._vault ? ('vault:' + k.name) : k.path;
      d.dataset.vault = k._vault ? '1' : '0';
      d.innerHTML = '<span style="color:' + (k._vault ? 'var(--accent)' : 'var(--text2)') + ';font-family:monospace">' + (k.type === 'ed25519' ? '&#9000;' : k.type === 'rsa' ? '&#9881;' : '&#128196;') + '</span>'
        + '<span style="flex:1;overflow:hidden;text-overflow:ellipsis">' + k.name + (k._vault ? ' <span style="color:var(--accent);font-size:9px">(vault)</span>' : '') + '</span>'
        + '<span style="color:var(--text3);font-size:10px">' + k.type + '</span>';
      list.appendChild(d);
    });
    if (allKeys.length > 0) showKeyDetails(allKeys[0]);
  } catch (e) { }
}

export function showKeyDetails(key) {
  if (!key) return;
  document.getElementById('selected-key-name').textContent = key.name;
  document.getElementById('selected-key-path').textContent = key.path;
  const pubEl = document.getElementById('selected-key-pub');
  const pub = (key.public_key || '').trim();
  if (pubEl) pubEl.textContent = pub || '(public key not found)';
  const typeEl = document.getElementById('selected-key-type');
  if (typeEl) typeEl.textContent = key.type || 'unknown';
  const fpEl = document.getElementById('selected-key-fingerprint');
  if (fpEl) {
    if (key.fingerprint) fpEl.textContent = key.fingerprint;
    else if (pub) fpEl.textContent = 'Unavailable (no fingerprint metadata)';
    else fpEl.textContent = 'Unavailable (public key missing)';
  }

  const detailsEl = document.getElementById('selected-key-path');
  if (detailsEl) {
    const source = key._vault ? 'vault' : 'filesystem';
    detailsEl.textContent = `${key.path} | source=${source} | type=${key.type || 'unknown'}`;
  }
}

export function setupKeys() {
  // Copy pubkey button
  document.getElementById('copy-pubkey-btn').onclick = () => {
    const el = document.getElementById('selected-key-pub');
    if (!el) return;
    const text = (el.textContent || '').trim();
    if (!text || text === '(public key not found)') return;
    navigator.clipboard.writeText(text);
  };

  // New key button
  document.getElementById('keys-new-btn').onclick = () => {
    document.getElementById('gen-key-modal-name').value = '';
    document.getElementById('gen-key-modal-type').value = 'ed25519';
    document.getElementById('gen-key-modal-pass').value = '';
    document.getElementById('generate-key-modal').style.display = 'flex';
    document.getElementById('gen-key-modal-name').focus();
  };

  // Refresh
  document.getElementById('keys-refresh-btn').onclick = loadKeys;

  // Copy public key
  document.getElementById('keys-copy-btn').onclick = () => {
    const pubEl = document.getElementById('selected-key-pub');
    const text = (pubEl?.textContent || '').trim();
    if (text && text !== '(public key not found)') {
      navigator.clipboard.writeText(text);
      return;
    }
    const selected = document.querySelector('.keys-list .key-item.selected');
    if (selected?.dataset?.key) navigator.clipboard.writeText(selected.dataset.key);
  };

  // Delete key
  document.getElementById('keys-delete-btn').onclick = () => {
    const selected = document.querySelector('.keys-list .key-item.selected');
    if (selected) {
      document.getElementById('delete-key-name').textContent = selected.dataset.key;
      document.getElementById('delete-key-modal').style.display = 'flex';
    }
  };

  // Generate Key Modal
  setupGenerateKeyModal();

  // Import Key Modal
  setupImportKeyModal();

  // Delete Key Modal
  setupDeleteKeyModal();

  // Key list selection
  document.getElementById('keys-list').addEventListener('click', (e) => {
    const item = e.target.closest('.key-item');
    if (item) {
      document.querySelectorAll('.keys-list .key-item').forEach(x => x.classList.remove('selected'));
      item.classList.add('selected');
      const key = _loadedKeys.find(k => k.name === item.dataset.key);
      if (key) showKeyDetails(key);
    }
  });
}

function setupGenerateKeyModal() {
  document.getElementById('gen-key-close').onclick = () => {
    document.getElementById('generate-key-modal').style.display = 'none';
  };
  document.getElementById('gen-key-cancel').onclick = () => {
    document.getElementById('generate-key-modal').style.display = 'none';
  };
  document.getElementById('gen-key-confirm').onclick = async () => {
    const name = document.getElementById('gen-key-modal-name').value.trim();
    const type = document.getElementById('gen-key-modal-type').value;
    if (!name) {
      document.getElementById('gen-key-modal-name').style.borderColor = 'var(--red)';
      return;
    }
    document.getElementById('gen-key-confirm').textContent = 'Generating...';
    try {
      const result = await window.rterm.generateKey(name, type, '');
      if (result.success) {
        document.getElementById('generate-key-modal').style.display = 'none';
        loadKeys();
      } else {
        alert('Key generation failed: ' + (result.error || 'unknown'));
      }
    } catch (e) {
      alert('Key generation error: ' + e);
    }
    document.getElementById('gen-key-confirm').textContent = 'Generate';
  };
  document.getElementById('generate-key-modal').addEventListener('click', (e) => {
    if (e.target === document.getElementById('generate-key-modal')) {
      document.getElementById('generate-key-modal').style.display = 'none';
    }
  });
}

function setupImportKeyModal() {
  document.getElementById('keys-import-btn').onclick = () => {
    document.getElementById('import-key-modal').style.display = 'flex';
    document.getElementById('ik-name').value = '';
    document.getElementById('ik-private').value = '';
    document.getElementById('ik-public').value = '';
    document.getElementById('ik-error').style.display = 'none';
    document.getElementById('ik-name').focus();
  };
  document.getElementById('ik-browse').onclick = () => {
    document.getElementById('ik-file').click();
  };
  document.getElementById('ik-file').onchange = function () {
    const file = this.files[0];
    if (!file) return;
    const reader = new FileReader();
    reader.onload = (e) => {
      const content = e.target.result;
      document.getElementById('ik-private').value = content;
      const name = file.name.replace(/\.(pem|key|priv)$/i, '');
      if (name && !document.getElementById('ik-name').value) {
        document.getElementById('ik-name').value = name;
      }
    };
    reader.readAsText(file);
    this.value = '';
  };
  document.getElementById('ik-close').onclick = () => {
    document.getElementById('import-key-modal').style.display = 'none';
  };
  document.getElementById('ik-cancel').onclick = () => {
    document.getElementById('import-key-modal').style.display = 'none';
  };
  document.getElementById('ik-import').onclick = async () => {
    const name = document.getElementById('ik-name').value.trim();
    const priv = document.getElementById('ik-private').value.trim();
    if (!name || !priv) {
      document.getElementById('ik-error').textContent = 'Name and private key are required';
      document.getElementById('ik-error').style.display = '';
      return;
    }
    if (!priv.includes('BEGIN') || !priv.includes('PRIVATE KEY')) {
      document.getElementById('ik-error').textContent = 'Invalid private key format';
      document.getElementById('ik-error').style.display = '';
      return;
    }
    const pub = document.getElementById('ik-public').value.trim();
    const pass = window.__rterm_vault_pass;
    if (!pass) {
      document.getElementById('ik-error').textContent = 'Vault is not unlocked. Open Settings and enable the vault first.';
      document.getElementById('ik-error').style.display = '';
      return;
    }
    document.getElementById('ik-import').textContent = 'Importing...';
    try {
      const result = await window.rterm.importVaultKey(name, priv, pub, '', pass);
      if (result.success) {
        document.getElementById('import-key-modal').style.display = 'none';
        const reload = await window.rterm.loadSessions(pass);
        if (reload.success) window.__rterm_vault_keys = reload.keys || [];
        loadKeys();
      } else {
        document.getElementById('ik-error').textContent = result.error || 'Import failed';
        document.getElementById('ik-error').style.display = '';
      }
    } catch (e) {
      document.getElementById('ik-error').textContent = 'Error: ' + e;
      document.getElementById('ik-error').style.display = '';
    }
    document.getElementById('ik-import').textContent = 'Import Key';
  };
}

function setupDeleteKeyModal() {
  document.getElementById('delete-key-cancel').onclick = () => {
    document.getElementById('delete-key-modal').style.display = 'none';
  };
  document.getElementById('delete-key-confirm').onclick = async () => {
    const selected = document.querySelector('.keys-list .key-item.selected');
    if (selected) {
      const isVault = selected.dataset.vault === '1';
      try {
        if (isVault) {
          const pass = window.__rterm_vault_pass;
          if (pass) await window.rterm.deleteVaultKey(selected.dataset.key, pass);
        } else {
          await window.rterm.deleteKey(selected.dataset.path);
        }
      } catch (e) { }
      if (isVault && window.__rterm_vault_pass) {
        const reload = await window.rterm.loadSessions(window.__rterm_vault_pass);
        if (reload.success) window.__rterm_vault_keys = reload.keys || [];
      }
      loadKeys();
    }
    document.getElementById('delete-key-modal').style.display = 'none';
  };
  document.getElementById('delete-key-modal').addEventListener('click', (e) => {
    if (e.target === document.getElementById('delete-key-modal')) {
      document.getElementById('delete-key-modal').style.display = 'none';
    }
  });
}
