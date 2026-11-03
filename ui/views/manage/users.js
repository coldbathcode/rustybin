// Management, Users and roles (access control): the users with their roles,
// status and last sign-in; Invite answers a one-time link to copy (shown
// once: the person opens it and sets their own password; Invite a portal
// reader gives the portal_reader role alone, whose link opens the API
// portal's join page, never the console); a user's roles,
// disable and enable, a new link (a password reset), delete. The roles with
// the permission matrix, privileged roles marked; custom roles saved and
// deleted. GET /_rustybin/users and /roles need users.read, changes
// users.manage; users exist only with the password sign-in (404 otherwise),
// roles in every mode. The same on every surface: rustybin users and roles,
// the *_user(s) and *_role(s) tools, users: and roles: of rustybin.yaml.
import { h, replace, card, field, input, notice, toast, icon, badge, spinner, copyButton } from '../../lib/dom.js';
import { SERVER, getJson, send } from '../../lib/http.js';
import * as perms from '../../lib/perms.js';

/** A date and time in the browser's locale (sign-ins, expiries days ahead). */
function when(iso) {
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? String(iso || '') : d.toLocaleString();
}

async function call(method, path, body) {
  const r = await send({
    control: true, admin: true, session: false, method, url: SERVER + path,
    headers: body ? { 'Content-Type': 'application/json' } : undefined,
    body: body ? JSON.stringify(body) : undefined,
  });
  if (r.error) throw new Error(r.error);
  if (!r.ok) throw new Error((r.json && r.json.error) || `${r.status} ${r.statusText}`);
  return r.json;
}

const STATUS = { active: 'ok', invited: 'info', disabled: 'warn' };

function roleChecks(roles, chosen, prefix) {
  return h('div', { class: 'row', style: { flexWrap: 'wrap', gap: '10px' } },
    roles.map((r) => h('label', { class: 'row nowrap small', title: r.description },
      h('input', { type: 'checkbox', name: 'role', value: r.name, id: prefix + '-' + r.name, checked: chosen.includes(r.name) }),
      h('span', { class: 'mono' }, r.name),
      r.privileged ? badge('privileged', 'warn') : null)));
}

function checkedRoles(el) {
  return Array.from(el.querySelectorAll('input[name=role]:checked')).map((i) => i.value);
}

function linkView(v) {
  const url = v.invitation.url;
  const box = input({ value: url, readOnly: true, id: 'invite-link', 'aria-label': 'Invitation link' });
  return notice('ok', h('div', { class: 'stack-sm' },
    h('strong', null, (v.invitation.reset ? 'A new link for ' : 'Invitation for ') + v.user.name + ' (shown once, valid until ' + when(v.invitation.expires_at) + ')'),
    h('div', { class: 'row nowrap' }, box, copyButton(() => url, { label: 'Copy link', toast: 'Link copied' })),
    h('span', { class: 'small muted' }, 'Send it to them: they open it and set their own password. Nobody else ever sees or types it.')));
}

export function usersPanel() {
  const el = h('div', { class: 'stack' });
  const usersEl = h('div', { id: 'users-list' }, spinner('Loading the users'));
  const rolesEl = h('div', { id: 'roles-list' }, spinner('Loading the roles'));
  const resultEl = h('div', { id: 'users-result' });
  let roles = [];
  let permissions = [];
  let usersOn = true;

  // ── Invite ──
  const name = input({ id: 'invite-name', placeholder: 'alice', autocomplete: 'off', maxlength: 64 });
  const display = input({ id: 'invite-display', placeholder: 'Alice Doe', maxlength: 100 });
  const email = input({ id: 'invite-email', type: 'email', placeholder: 'alice@example.com' });
  const inviteRoles = h('div', { id: 'invite-roles' });
  const inviteProjects = h('textarea', { class: 'input mono', id: 'invite-projects', rows: 2, placeholder: 'payments=editor' });
  const ssoBox = h('input', { type: 'checkbox', id: 'invite-sso' });
  const ssoField = h('label', { class: 'row small', style: { gap: '6px' } }, ssoBox,
    'Single sign-on user: no link and no password (with sso.provision off, add people here before their first sign-in; the roles are pinned on top of their groups\')');
  const inviteBtn = perms.guard(h('button', { class: 'btn primary', type: 'submit', id: 'invite-submit' }, icon('plus'), 'Invite'), 'users.manage');
  // A tester's login: the portal_reader role alone, the portal's join link.
  const readerBtn = perms.guard(h('button', { class: 'btn', type: 'button', id: 'invite-portal-reader', title: 'The portal_reader role alone: the link opens the API portal, and the login opens nothing in the console' }, icon('lock'), 'Invite a portal reader'), 'users.manage');
  readerBtn.addEventListener('click', () => {
    for (const i of inviteRoles.querySelectorAll('input[name=role]')) i.checked = i.value === 'portal_reader';
    if (typeof inviteForm.requestSubmit === 'function') inviteForm.requestSubmit(inviteBtn);
    else inviteBtn.click();
  });
  const inviteForm = h('form', { class: 'stack', 'aria-label': 'Invite a user' },
    h('div', { class: 'row', style: { gap: '12px', flexWrap: 'wrap' } },
      field('Sign-in name', name, 'Letters, digits and . _ @ -'),
      field('Display name', display),
      field('Email', email, 'Optional, never logged')),
    field('Roles', inviteRoles, 'You grant only roles whose permissions you hold.'),
    field('Roles in projects', inviteProjects, 'One project per line: payments=editor (with some, the instance roles may be left empty: the user sees those projects only).'),
    ssoField,
    h('div', { class: 'row' }, h('span', { class: 'grow muted small' }, 'The user is created invited and joins through the link. A portal reader reads the API portal\'s closed items and nothing in the console.'), readerBtn, inviteBtn));
  inviteForm.addEventListener('submit', async (e) => {
    e.preventDefault();
    const chosen = checkedRoles(inviteRoles);
    const inProjects = parseBindings(inviteProjects.value, 'project');
    if (!name.value.trim() || (!chosen.length && !inProjects.length)) {
      replace(resultEl, notice('err', 'Give a sign-in name and at least one role (or roles in a project).'));
      return;
    }
    inviteBtn.disabled = true;
    try {
      const body = { name: name.value.trim(), roles: chosen };
      if (inProjects.length) body.projects = inProjects;
      if (display.value.trim()) body.display_name = display.value.trim();
      if (email.value.trim()) body.email = email.value.trim();
      if (ssoBox.checked) body.sso = true;
      const v = await call('POST', '/_rustybin/users', body);
      if (v.invitation) replace(resultEl, linkView(v));
      else replace(resultEl, notice('ok', icon('check'), v.user.name + ' signs in with single sign-on: no link to send.'));
      ssoBox.checked = false;
      name.value = ''; display.value = ''; email.value = ''; inviteProjects.value = '';
      loadUsers();
    } catch (err) {
      replace(resultEl, notice('err', 'The invitation failed: ' + err.message));
    } finally {
      inviteBtn.disabled = !perms.can('users.manage');
    }
  });

  // ── Users ──
  async function act(label, fn) {
    try {
      const v = await fn();
      if (v && v.invitation) replace(resultEl, linkView(v));
      else toast(label);
      loadUsers();
      loadRoles();
    } catch (err) {
      replace(resultEl, notice('err', err.message));
    }
  }

  function rolesEditor(u, row) {
    // A single sign-on user's roles come from their groups: an admin pins
    // extra ones.
    const sso = u.source === 'sso';
    const checks = roleChecks(roles, sso ? (u.pinned_roles || []) : u.roles, 'user-role');
    const save = h('button', { class: 'btn sm primary', type: 'submit', id: 'user-roles-save' }, sso ? 'Save pinned roles' : 'Save roles');
    const form = h('form', { class: 'stack-sm', id: 'user-roles-form', 'aria-label': 'Roles of ' + u.name },
      sso ? h('p', { class: 'muted small', style: { margin: 0 } }, 'From the provider\'s groups: ' + ((u.sso && u.sso.mapped_roles) || []).join(', ') + '. Pinned roles stay on top of them at every sign-in.') : null,
      checks,
      h('div', { class: 'row' }, h('span', { class: 'grow' }), h('button', { class: 'btn sm', type: 'button', onclick: () => loadUsers() }, 'Cancel'), save));
    form.addEventListener('submit', (e) => {
      e.preventDefault();
      act('Roles of ' + u.name + ' changed', () => call('PATCH', '/_rustybin/users/' + encodeURIComponent(u.id), sso ? { pinned_roles: checkedRoles(form) } : { roles: checkedRoles(form) }));
    });
    row.after(h('tr', { class: 'user-roles-row' }, h('td', { colspan: 6 }, form)));
  }

  function userRow(u) {
    const manage = (b) => perms.guard(b, 'users.manage');
    const row = h('tr', { 'data-user': u.name },
      h('td', null, h('div', { class: 'mono' }, u.name), u.display_name !== u.name ? h('div', { class: 'small muted' }, u.display_name) : null,
        u.first_admin ? h('div', null, badge('first admin', 'info')) : null,
        u.source === 'sso' ? h('div', null, badge('single sign-on', 'violet')) : null),
      h('td', null, h('div', { class: 'row', style: { gap: '4px', flexWrap: 'wrap' } }, u.roles.map((r) => badge(r, roles.find((x) => x.name === r && x.privileged) ? 'warn' : ''))),
        (u.projects || []).length ? h('div', { class: 'row user-bindings', style: { gap: '4px', flexWrap: 'wrap', marginTop: '4px' } },
          u.projects.map((b) => h('span', { class: 'badge violet', 'data-binding': b.project, title: b.sso ? 'From a single sign-on group' : 'A member of the project' }, b.project + ': ' + b.roles.join(' ')))) : null),
      h('td', null, badge(u.status, STATUS[u.status] || ''), u.invitation ? h('div', { class: 'tiny muted' }, 'link until ' + when(u.invitation.expires_at)) : null),
      h('td', { class: 'small', title: u.last_sign_in || '' }, u.last_sign_in ? when(u.last_sign_in) : 'never'),
      h('td', { class: 'small' }, String(u.sessions)),
      h('td', null, h('div', { class: 'row nowrap' })));
    const actions = row.lastChild.firstChild;
    const buttons = [
      manage(h('button', { class: 'btn sm', type: 'button', 'data-action': 'roles', onclick: () => rolesEditor(u, row) }, 'Roles')),
      u.status === 'disabled'
        ? manage(h('button', { class: 'btn sm', type: 'button', 'data-action': 'enable', onclick: () => act(u.name + ' enabled', () => call('PATCH', '/_rustybin/users/' + encodeURIComponent(u.id), { disabled: false })) }, 'Enable'))
        : manage(h('button', { class: 'btn sm', type: 'button', 'data-action': 'disable', onclick: () => {
          if (!window.confirm('Disable ' + u.name + '? Their sessions end and their sign-in is refused.')) return;
          act(u.name + ' disabled', () => call('PATCH', '/_rustybin/users/' + encodeURIComponent(u.id), { disabled: true }));
        } }, 'Disable')),
      u.status === 'disabled' || u.source === 'sso' ? null : manage(h('button', { class: 'btn sm', type: 'button', 'data-action': 'link', title: u.status === 'invited' ? 'A new invitation link' : 'A password reset: a new link; their password works until it is accepted', onclick: () => act('A new link', () => call('POST', '/_rustybin/users/' + encodeURIComponent(u.id) + '/invite')) }, icon('link'), u.status === 'invited' ? 'New link' : 'Reset')),
      u.first_admin ? null : manage(h('button', { class: 'btn sm', type: 'button', title: 'Delete', 'aria-label': 'Delete ' + u.name, 'data-action': 'delete', onclick: () => {
        if (!window.confirm('Delete ' + u.name + '? They can no longer sign in.')) return;
        act(u.name + ' deleted', () => call('DELETE', '/_rustybin/users/' + encodeURIComponent(u.id)));
      } }, icon('trash')))];
    actions.append(...buttons.filter(Boolean));
    return row;
  }

  async function loadUsers() {
    try {
      const d = await getJson('/_rustybin/users', { session: false });
      usersOn = true;
      replace(usersEl,
        h('p', { class: 'muted small' }, d.count + ' user(s) of ' + d.capacity + '; ' + d.open_invitations + ' open invitation(s) of ' + d.max_open_invitations + '.'),
        h('div', { class: 'table-wrap' }, h('table', { class: 'table', id: 'users-table' },
          h('thead', null, h('tr', null, h('th', null, 'User'), h('th', null, 'Roles'), h('th', null, 'Status'), h('th', null, 'Last sign-in'), h('th', null, 'Sessions'), h('th', null, ''))),
          h('tbody', null, d.users.map(userRow)))));
    } catch (e) {
      const msg = e && e.message ? e.message : String(e);
      if (/^404/.test(msg)) {
        usersOn = false;
        replace(usersEl, notice('', 'Users sign in only with the password sign-in (RUSTYBIN_CONTROL_AUTH=password, the default). The roles below still apply to control JWTs that name them (rb_roles).'));
      } else {
        replace(usersEl, notice(/^403/.test(msg) ? 'warn' : 'err', /^403/.test(msg) ? 'Needs the users.read permission, which your roles do not grant.' : 'The users could not be loaded: ' + msg));
      }
    }
    inviteForm.hidden = !usersOn;
  }

  // ── Roles: the matrix, custom roles ──
  const roleName = input({ id: 'role-name', placeholder: 'catalog-reader', maxlength: 40 });
  const roleDesc = input({ id: 'role-description', placeholder: 'What the role is for', maxlength: 200 });
  const rolePerms = h('div', { id: 'role-permissions' });
  const roleSave = perms.guard(h('button', { class: 'btn primary', type: 'submit', id: 'role-save' }, 'Save role'), 'users.manage');
  const roleForm = h('form', { class: 'stack', 'aria-label': 'A custom role' },
    h('div', { class: 'row', style: { gap: '12px', flexWrap: 'wrap' } },
      field('Name', roleName, 'Lower case letters, digits, _ and -; an existing custom role is changed'),
      field('Description', roleDesc)),
    field('Permissions', rolePerms, 'users.manage or settings.security make the role privileged.'),
    h('div', { class: 'row' }, h('span', { class: 'grow' }), roleSave));
  roleForm.addEventListener('submit', async (e) => {
    e.preventDefault();
    const chosen = Array.from(rolePerms.querySelectorAll('input[name=permission]:checked')).map((i) => i.value);
    try {
      const v = await call('PUT', '/_rustybin/roles/' + encodeURIComponent(roleName.value.trim()), { description: roleDesc.value, permissions: chosen });
      toast((v.created ? 'Created' : 'Changed') + ' role ' + v.name);
      loadRoles();
    } catch (err) {
      replace(resultEl, notice('err', 'The role was not saved: ' + err.message));
    }
  });

  function fillRoleForm(r) {
    roleName.value = r ? r.name : '';
    roleDesc.value = r ? r.description : '';
    for (const i of rolePerms.querySelectorAll('input[name=permission]')) i.checked = !!(r && r.permissions.includes(i.value));
  }

  function matrix() {
    const groups = [];
    for (const p of permissions) {
      let g = groups.find((x) => x.name === p.group);
      if (!g) { g = { name: p.group, perms: [] }; groups.push(g); }
      g.perms.push(p);
    }
    const head = h('tr', null, h('th', null, 'Permission'), roles.map((r) => h('th', { class: 'small', title: r.description },
      h('div', { class: 'mono' }, r.name), r.privileged ? badge('privileged', 'warn') : null, r.built_in ? null : badge('custom', 'info'),
      h('div', { class: 'tiny muted' }, r.users + ' user(s)'),
      r.built_in ? null : h('div', { class: 'row nowrap' },
        h('button', { class: 'btn sm ghost', type: 'button', title: 'Edit', 'aria-label': 'Edit ' + r.name, onclick: () => { fillRoleForm(r); roleName.focus(); } }, icon('edit')),
        perms.guard(h('button', { class: 'btn sm ghost', type: 'button', title: 'Delete', 'aria-label': 'Delete role ' + r.name, 'data-delete-role': r.name, onclick: async () => {
          if (!window.confirm('Delete the role ' + r.name + '?')) return;
          try { await call('DELETE', '/_rustybin/roles/' + encodeURIComponent(r.name)); toast('Role ' + r.name + ' deleted'); loadRoles(); } catch (err) { replace(resultEl, notice('err', err.message)); }
        } }, icon('trash')), 'users.manage')))));
    const rows = [];
    for (const g of groups) {
      rows.push(h('tr', { class: 'group-row' }, h('th', { colspan: roles.length + 1, class: 'small' }, g.name)));
      for (const p of g.perms) {
        rows.push(h('tr', null, h('td', { title: p.summary }, h('span', { class: 'mono small' }, p.name)),
          roles.map((r) => h('td', { class: 'small', 'aria-label': r.name + (r.permissions.includes(p.name) ? ' has ' : ' lacks ') + p.name },
            r.permissions.includes(p.name) ? icon('check') : ''))));
      }
    }
    return h('div', { class: 'table-wrap' }, h('table', { class: 'table compact', id: 'roles-matrix' }, h('thead', null, head), h('tbody', null, rows)));
  }

  async function loadRoles() {
    try {
      const d = await getJson('/_rustybin/roles', { session: false });
      roles = d.roles;
      permissions = d.permissions;
      replace(inviteRoles, roleChecks(roles, [], 'invite-role'));
      replace(rolePerms, h('div', { class: 'row', style: { flexWrap: 'wrap', gap: '10px' } }, permissions.map((p) =>
        h('label', { class: 'row nowrap small', title: p.summary }, h('input', { type: 'checkbox', name: 'permission', value: p.name }), h('span', { class: 'mono' }, p.name)))));
      replace(rolesEl, h('p', { class: 'muted small' }, d.count + ' role(s): the five built-in ones never change; ' + d.custom + ' custom of ' + d.max_custom_roles + '.'), matrix());
    } catch (e) {
      const msg = e && e.message ? e.message : String(e);
      replace(rolesEl, notice(/^403/.test(msg) ? 'warn' : 'err', /^403/.test(msg) ? 'Needs the users.read permission, which your roles do not grant.' : 'The roles could not be loaded: ' + msg));
    }
  }

  // ── Projects: roles on part of the instance (step 11) ──
  const projectsEl = h('div', { id: 'projects-list' }, spinner('Loading the projects'));
  const projName = input({ id: 'project-name', placeholder: 'payments', maxlength: 40 });
  const projDesc = input({ id: 'project-description', placeholder: 'The payments team', maxlength: 200 });
  const projMembers = h('textarea', { class: 'input mono', id: 'project-members', rows: 4, placeholder: 'alice=editor\nbob=viewer tester' });
  const projSave = perms.guard(h('button', { class: 'btn primary', type: 'submit', id: 'project-save' }, 'Save project'), 'users.manage');
  const projForm = h('form', { class: 'stack', 'aria-label': 'A project' },
    h('div', { class: 'row', style: { gap: '12px', flexWrap: 'wrap' } },
      field('Name', projName, 'The mock name rules: lower case letters, digits and -'),
      field('Description', projDesc)),
    field('Members', projMembers, 'One per line: user=role role (the whole list). Their roles apply to the project\'s mocks, saved tests and checkpoints, on top of their instance roles.'),
    h('div', { class: 'row' }, h('span', { class: 'grow' }), projSave));
  projForm.addEventListener('submit', async (e) => {
    e.preventDefault();
    const n = projName.value.trim();
    if (!n) { replace(resultEl, notice('err', 'Give the project a name.')); return; }
    try {
      const v = await call('PUT', '/_rustybin/projects/' + encodeURIComponent(n), { description: projDesc.value, members: parseBindings(projMembers.value, 'user') });
      toast((v.created ? 'Created' : 'Saved') + ' project ' + v.name);
      projName.value = ''; projDesc.value = ''; projMembers.value = '';
      loadProjects();
      loadUsers();
    } catch (err) {
      replace(resultEl, notice('err', 'The project was not saved: ' + err.message));
    }
  });

  function projectRow(p) {
    const held = p.resources || {};
    const count = (k) => (held[k] || []).length;
    return h('tr', { 'data-project': p.name },
      h('td', null, h('div', { class: 'mono' }, p.name), p.description ? h('div', { class: 'small muted' }, p.description) : null),
      h('td', { class: 'small' }, (p.members || []).map((m) => h('div', { class: 'mono' }, (m.user || m.user_id) + ': ' + m.roles.join(' ')))),
      h('td', { class: 'small' }, count('mocks') + ' mock(s), ' + count('tests') + ' test(s), ' + count('checkpoints') + ' checkpoint(s)'),
      h('td', null, h('div', { class: 'row nowrap' },
        h('button', { class: 'btn sm ghost', type: 'button', title: 'Edit', 'aria-label': 'Edit ' + p.name, onclick: () => {
          projName.value = p.name; projDesc.value = p.description || '';
          projMembers.value = (p.members || []).map((m) => (m.user || m.user_id) + '=' + m.roles.join(' ')).join('\n');
          projName.focus();
        } }, icon('edit')),
        perms.guard(h('button', { class: 'btn sm ghost', type: 'button', title: 'Delete', 'aria-label': 'Delete project ' + p.name, 'data-delete-project': p.name, onclick: async () => {
          const holds = count('mocks') + count('tests') + count('checkpoints') > 0;
          let q = '';
          if (holds) {
            const to = window.prompt('The project ' + p.name + ' holds resources: where do they go? (another project, or instance)', 'instance');
            if (!to) return;
            q = '?move_to=' + encodeURIComponent(to);
          } else if (!window.confirm('Delete the project ' + p.name + '? Its members lose their roles there.')) return;
          try { await call('DELETE', '/_rustybin/projects/' + encodeURIComponent(p.name) + q); toast('Project ' + p.name + ' deleted'); loadProjects(); loadUsers(); } catch (err) { replace(resultEl, notice('err', err.message)); }
        } }, icon('trash')), 'users.manage'))));
  }

  async function loadProjects() {
    try {
      const d = await getJson('/_rustybin/projects', { session: false });
      replace(projectsEl,
        h('p', { class: 'muted small' }, d.count + ' project(s) of ' + d.capacity + '.'),
        d.projects.length ? h('div', { class: 'table-wrap' }, h('table', { class: 'table', id: 'projects-table' },
          h('thead', null, h('tr', null, h('th', null, 'Project'), h('th', null, 'Members'), h('th', null, 'Holds'), h('th', null, ''))),
          h('tbody', null, d.projects.map(projectRow)))) : null);
    } catch (e) {
      replace(projectsEl, notice('err', 'The projects could not be loaded: ' + (e && e.message ? e.message : e)));
    }
  }

  for (const c of [name, display, email, roleName, roleDesc, inviteProjects, projName, projDesc, projMembers]) if (!perms.can('users.manage')) c.disabled = true;
  el.append(
    card('Users', { hint: 'who signs in, with which roles', actions: h('button', { class: 'btn sm ghost', type: 'button', onclick: () => { loadUsers(); loadRoles(); loadProjects(); } }, icon('refresh'), 'Refresh') }, usersEl),
    card('Invite a user', { hint: 'a one-time link, 72 hours' }, inviteForm, resultEl),
    card('Projects', { hint: 'roles on part of the instance' }, projectsEl, projForm),
    card('Roles', { hint: 'what each role may do' }, rolesEl),
    card('A custom role', { hint: 'create or change' }, roleForm));
  loadRoles().then(loadUsers);
  loadProjects();
  return el;
}

/** `name=role role` lines as [{key: name, roles}] (`key` is project or user). */
function parseBindings(text, key) {
  return String(text || '').split('\n').map((l) => l.trim()).filter(Boolean).map((l) => {
    const i = l.lastIndexOf('=');
    const who = i < 0 ? l : l.slice(0, i).trim();
    const roles = i < 0 ? [] : l.slice(i + 1).split(/[\s,+]+/).filter(Boolean);
    return { [key]: who, roles };
  });
}

