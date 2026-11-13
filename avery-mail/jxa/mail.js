function run(argv) {
  var req = JSON.parse(argv[0]);
  try {
    return JSON.stringify({ok: true, data: dispatch(req)});
  } catch (e) {
    return JSON.stringify({ok: false, error: String((e && e.message) || e)});
  }
}

function mail() { return Application('Mail'); }
function pad(n) { return (n < 10 ? '0' : '') + n; }
function fmt(d) {
  return d.getFullYear() + '-' + pad(d.getMonth() + 1) + '-' + pad(d.getDate()) +
    ' ' + pad(d.getHours()) + ':' + pad(d.getMinutes());
}

var INBOX_NAMES = ['inbox', 'posteingang', 'boite de reception', 'bo\u00eete de r\u00e9ception',
                   'posta in arrivo', 'bandeja de entrada', 'postvak in'];

function dispatch(req) {
  if (req.op === 'ping') { return {jxa: true}; }
  if (req.op === 'approval_test') {
    return {decision: askApproval({
      title: 'Selbsttest: Freigabedialog', note: 'Nur ein Test. Es wird nichts gesendet.',
      sendLabel: 'Weiter', from: 'test@example.invalid', to: ['test@example.invalid'], cc: [],
      subject: 'Testdialog', body: 'Der Dialog funktioniert, wenn du diesen Text lesen kannst.',
      reply: null}, req.seconds || 20)};
  }
  if (req.op === 'accounts') {
    var all = mail().accounts(), out = [];
    for (var i = 0; i < all.length; i++) { out.push(accountInfo(all[i])); }
    return out;
  }
  var acc = accountById(req.accountId);
  if (req.op === 'account_info') { return accountInfo(acc); }
  if (req.op === 'mailboxes') { return listMailboxes(acc, req.depth || 2); }
  if (req.op === 'messages') { return queryMessages(acc, req); }
  if (req.op === 'read') { return readMessage(acc, req); }
  if (req.op === 'compose') { return composeMessage(acc, req); }
  throw new Error('unknown_op');
}

function accountInfo(a) {
  var addrs = [], type = '';
  try { addrs = a.emailAddresses(); } catch (e) {}
  try { type = String(a.accountType()); } catch (e) {}
  return {id: a.id(), name: a.name(), type: type, addresses: addrs};
}

function accountById(id) {
  var accs = mail().accounts();
  for (var i = 0; i < accs.length; i++) {
    if (accs[i].id() === id) { return accs[i]; }
  }
  throw new Error('account_not_found');
}

function findMailbox(container, wanted, prefix, depth) {
  var names = container.mailboxes.name();
  var lc = wanted ? String(wanted).toLowerCase() : 'inbox';
  var inboxMode = (lc === 'inbox');
  var i, nl, path, found;
  for (i = 0; i < names.length; i++) {
    nl = String(names[i]).toLowerCase();
    path = prefix ? prefix + '/' + names[i] : String(names[i]);
    if (inboxMode ? INBOX_NAMES.indexOf(nl) >= 0 : (nl === lc || path.toLowerCase() === lc)) {
      return {ref: container.mailboxes[i], path: path};
    }
  }
  if (depth > 0) {
    for (i = 0; i < names.length; i++) {
      path = prefix ? prefix + '/' + names[i] : String(names[i]);
      found = findMailbox(container.mailboxes[i], wanted, path, depth - 1);
      if (found) { return found; }
    }
  }
  return null;
}

function listMailboxes(acc, depth) {
  var out = [];
  function walk(container, prefix, d) {
    var names = container.mailboxes.name();
    var unread = container.mailboxes.unreadCount();
    for (var i = 0; i < names.length && out.length < 200; i++) {
      var path = prefix ? prefix + '/' + names[i] : String(names[i]);
      out.push({path: path, unread: unread[i]});
      if (d > 0) { walk(container.mailboxes[i], path, d - 1); }
    }
  }
  walk(acc, '', depth);
  return out;
}

function queryMessages(acc, req) {
  var mb = findMailbox(acc, req.mailbox, '', 3);
  if (!mb) { throw new Error('mailbox_not_found'); }
  var conds = [];
  if (req.sinceDays) {
    conds.push({dateReceived: {_greaterThan: new Date(Date.now() - req.sinceDays * 86400000)}});
  }
  if (req.unreadOnly) { conds.push({readStatus: false}); }
  if (req.query) {
    if (req.field === 'subject') { conds.push({subject: {_contains: req.query}}); }
    else if (req.field === 'sender') { conds.push({sender: {_contains: req.query}}); }
    else { conds.push({_or: [{subject: {_contains: req.query}}, {sender: {_contains: req.query}}]}); }
  }
  var msgs = mb.ref.messages;
  var sel = msgs;
  if (conds.length === 1) { sel = msgs.whose(conds[0]); }
  else if (conds.length > 1) { sel = msgs.whose({_and: conds}); }
  var ids = sel.id();
  if (ids.length > req.maxHits) { throw new Error('too_many_hits:' + ids.length); }
  if (ids.length === 0) { return {mailbox: mb.path, total: 0, items: []}; }
  var subj = sel.subject(), snd = sel.sender(), dt = sel.dateReceived();
  var rd = sel.readStatus(), fl = sel.flaggedStatus();
  if (subj.length !== ids.length || snd.length !== ids.length || dt.length !== ids.length ||
      rd.length !== ids.length || fl.length !== ids.length) {
    throw new Error('mailbox_changed_during_read');
  }
  var items = [];
  for (var i = 0; i < ids.length; i++) {
    items.push({id: ids[i], subject: subj[i], from: snd[i], ts: Math.floor(dt[i].getTime() / 1000),
                date: fmt(dt[i]), read: rd[i], flagged: fl[i]});
  }
  items.sort(function (a, b) { return b.ts - a.ts; });
  return {mailbox: mb.path, total: ids.length, items: items.slice(0, req.limit)};
}

function recipients(coll) {
  var res = [];
  try {
    var n = coll.name(), a = coll.address();
    for (var i = 0; i < a.length; i++) { res.push({name: n[i] || '', address: a[i] || ''}); }
  } catch (e) {}
  return res;
}

function attachments(m) {
  var res = [];
  try {
    var n = m.mailAttachments.name(), s = m.mailAttachments.fileSize();
    for (var i = 0; i < n.length; i++) { res.push({name: n[i], size: s[i]}); }
  } catch (e) {}
  return res;
}

function readMessage(acc, req) {
  var mb = findMailbox(acc, req.mailbox, '', 3);
  if (!mb) { throw new Error('mailbox_not_found'); }
  var m = mb.ref.messages.byId(req.id);
  if (!ownedBy(m, acc)) { throw new Error('message_not_in_account'); }
  var out = {mailbox: mb.path, id: req.id};
  out.subject = m.subject();
  out.from = m.sender();
  var received = m.dateReceived();
  out.ts = Math.floor(received.getTime() / 1000);
  out.date = fmt(received);
  out.read = m.readStatus();
  out.to = recipients(m.toRecipients);
  out.cc = recipients(m.ccRecipients);
  out.attachments = attachments(m);
  var body = m.content();
  body = (body === null || body === undefined) ? '' : String(body);
  out.bodyChars = body.length;
  out.body = body.length > req.maxChars ? body.substring(0, req.maxChars) : body;
  return out;
}

function sleepSeconds(n) {
  try {
    var sa = Application.currentApplication();
    sa.includeStandardAdditions = true;
    sa.delay(n);
  } catch (e) {}
}

function addrOf(text) {
  var s = String(text || ''), m = s.match(/<([^<>]+)>/);
  return (m ? m[1] : s).trim().toLowerCase();
}

function rcptList(coll) {
  var res = [];
  try {
    var a = coll.address();
    for (var i = 0; i < a.length; i++) { res.push(String(a[i])); }
  } catch (e) {}
  return res;
}

function discard(msg) {
  try { mail().delete(msg); } catch (e) {}
}

function hidden(cp) {
  for (var k = 0; k < INVISIBLE_RANGES.length; k++) {
    if (cp >= INVISIBLE_RANGES[k][0] && cp <= INVISIBLE_RANGES[k][1]) { return true; }
  }
  return false;
}

function shown(value) {
  var s = String(value === null || value === undefined ? '' : value), out = '', i, cp;
  for (i = 0; i < s.length; i++) {
    cp = s.codePointAt(i);
    if (cp > 0xffff) { i++; }
    if (!hidden(cp)) { out += String.fromCodePoint(cp); }
  }
  return out;
}

function flat(value, max) {
  var chars = Array.from(shown(String(value === null || value === undefined ? '' : value).replace(/[\s\u0085]+/g, ' ')).trim());
  return chars.length > max ? chars.slice(0, max - 1).join('') + '\u2026' : chars.join('');
}

var ADDRESS = /^[A-Za-z0-9.!#$%&'*+\/=?^_`{|}~-]{1,64}@[A-Za-z0-9.-]{3,189}$/;

function plainAddress(a) {
  return a.length <= 254 && ADDRESS.test(a);
}

function ownedBy(m, acc) {
  var owner;
  try { owner = String(m.mailbox().account().id()); } catch (e) { return true; }
  return owner === String(acc.id());
}

function snapshot(msg) {
  var parts = [], reads = [
    function () { return String(msg.sender()); },
    function () { return rcptList(msg.toRecipients); },
    function () { return rcptList(msg.ccRecipients); },
    function () { return String(msg.subject()); },
    function () { return squash(msg.content()); }
  ];
  for (var i = 0; i < reads.length; i++) {
    try { parts.push(reads[i]()); } catch (e) { parts.push('?' + i); }
  }
  return JSON.stringify(parts);
}

function squash(value) {
  return String(value === null || value === undefined ? '' : value).replace(/\s+/g, '');
}

function flatList(list) {
  var out = [];
  for (var i = 0; i < list.length; i++) { out.push(flat(list[i], 300)); }
  return out.join(', ');
}

function askApproval(p, seconds) {
  var app = Application.currentApplication();
  app.includeStandardAdditions = true;
  try { app.activate(); } catch (e) {}
  var label = p.sendLabel || 'Senden';
  var lines = [p.note || 'Pr\u00fcfe Empf\u00e4nger und Text. Nur senden, wenn du das gerade selbst veranlasst hast.', ''];
  lines.push('Von:        ' + flat(p.from, 200));
  lines.push('An:         ' + flatList(p.to));
  if (p.cc.length) { lines.push('Cc:         ' + flatList(p.cc)); }
  lines.push('Betreff:    ' + flat(p.subject, 300));
  if (p.reply) { lines.push('Antwort auf: ' + flat(p.reply.subject, 150) + ' (von ' + flat(p.reply.from, 100) + ')'); }
  lines.push('', '--------------------------------', p.body, '--------------------------------');
  var answer;
  try {
    answer = app.displayDialog(lines.join('\n'), {
      withTitle: p.title || 'Goose m\u00f6chte eine E-Mail senden',
      buttons: ['Abbrechen', label],
      defaultButton: 'Abbrechen',
      cancelButton: 'Abbrechen',
      withIcon: 'caution',
      givingUpAfter: seconds
    });
  } catch (e) {
    var text = String((e && e.message) || e);
    if (text.indexOf('-128') >= 0) { return 'declined'; }
    throw new Error('approval_dialog_failed: ' + text);
  }
  if (answer.gaveUp) { return 'timeout'; }
  return answer.buttonReturned === label ? 'send' : 'declined';
}

function composeMessage(acc, req) {
  var app = mail(), addrs = [], fullName = '', msg, replyInfo = null, i;
  try { addrs = acc.emailAddresses(); } catch (e) {}
  if (!addrs.length) { throw new Error('no_sender_address'); }
  try { fullName = String(acc.fullName() || ''); } catch (e) {}
  var own = String(addrs[0]);
  var sender = (fullName && !/[,;<>"]/.test(fullName)) ? fullName + ' <' + own + '>' : own;
  var draft = (req.mode === 'draft');
  if (req.reply) {
    var mb = findMailbox(acc, req.reply.mailbox, '', 3);
    if (!mb) { throw new Error('mailbox_not_found'); }
    var orig = mb.ref.messages.byId(req.reply.id);
    if (!ownedBy(orig, acc)) { throw new Error('message_not_in_account'); }
    replyInfo = {subject: String(orig.subject()), from: String(orig.sender())};
    msg = app.reply(orig, {openingWindow: draft, replyToAll: !!req.reply.all});
    var quoted = '';
    for (i = 0; i < 20 && !quoted; i++) {
      try { quoted = String(msg.content() || ''); } catch (e) {}
      if (!quoted) { sleepSeconds(0.25); }
    }
    msg.content = draft ? req.body + (quoted ? '\n\n' + quoted : '') : req.body;
  } else {
    msg = app.OutgoingMessage({subject: req.subject, content: req.body, visible: draft, sender: sender});
    app.outgoingMessages.push(msg);
    for (i = 0; i < req.to.length; i++) { msg.toRecipients.push(app.ToRecipient({address: req.to[i]})); }
    for (i = 0; i < req.cc.length; i++) { msg.ccRecipients.push(app.CcRecipient({address: req.cc[i]})); }
  }
  var from = String(msg.sender());
  var known = [];
  for (i = 0; i < addrs.length; i++) { known.push(String(addrs[i]).toLowerCase()); }
  if (known.indexOf(addrOf(from)) < 0) { discard(msg); throw new Error('sender_mismatch'); }
  var preview = {from: from, to: rcptList(msg.toRecipients), cc: rcptList(msg.ccRecipients),
                 subject: String(msg.subject()), body: req.body, reply: replyInfo};
  if (!preview.to.length) { discard(msg); throw new Error('no_recipients'); }
  if (!draft) {
    var everyone = preview.to.concat(preview.cc);
    if (!(everyone.length <= req.maxRecipients)) { discard(msg); throw new Error('too_many_recipients'); }
    for (i = 0; i < everyone.length; i++) {
      if (!plainAddress(everyone[i])) { discard(msg); throw new Error('recipient_invalid'); }
    }
  }
  if (draft) {
    var saved = true;
    try { msg.save(); } catch (e) { saved = false; }
    preview.status = 'drafted';
    preview.saved = saved;
    return preview;
  }
  var finalText = '', want = squash(req.body);
  for (i = 0; i < 8; i++) {
    try { finalText = String(msg.content() || ''); } catch (e) { finalText = ''; }
    if (squash(finalText).indexOf(want) >= 0) { break; }
    sleepSeconds(0.25);
  }
  if (squash(finalText).indexOf(want) < 0 || squash(finalText).length > want.length + 1500) {
    discard(msg);
    throw new Error('content_mismatch');
  }
  preview.body = finalText;
  var approved = snapshot(msg), decision;
  try {
    decision = askApproval(preview, req.seconds);
  } catch (e) {
    discard(msg);
    throw e;
  }
  if (decision !== 'send') { discard(msg); preview.status = decision; return preview; }
  if (snapshot(msg) !== approved) { discard(msg); throw new Error('content_mismatch'); }
  try {
    msg.send();
  } catch (e) {
    throw new Error('send_state_unknown: ' + String((e && e.message) || e));
  }
  preview.status = 'sent';
  return preview;
}
