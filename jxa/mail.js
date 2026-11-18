function run(argv) {
  var req = JSON.parse(argv[0]);
  try {
    return JSON.stringify({ok: true, data: dispatch(req)});
  } catch (e) {
    return JSON.stringify({ok: false, error: String((e && e.message) || e)});
  }
}

function mail() { return Application('Mail'); }

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
  if (req.op === 'move') { return moveMessages(acc, req); }
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

function textClause(req) {
  if (req.field === 'subject') { return {subject: {_contains: req.query}}; }
  if (req.field === 'sender') { return {sender: {_contains: req.query}}; }
  return {_or: [{subject: {_contains: req.query}}, {sender: {_contains: req.query}}]};
}

function searchIds(msgs, req) {
  var conds = [], found, wanted = {}, i;
  if (req.sinceDays) {
    conds.push({dateReceived: {_greaterThan: new Date(Date.now() - req.sinceDays * 86400000)}});
  }
  if (req.unreadOnly) { conds.push({readStatus: false}); }
  conds.push(textClause(req));
  found = msgs.whose(conds.length === 1 ? conds[0] : {_and: conds}).id();
  for (i = 0; i < found.length; i++) { wanted[found[i]] = true; }
  return wanted;
}

function listedRow(msgs, row) {
  var m = msgs.byId(row.id), when, item;
  try {
    when = m.dateReceived();
    item = {id: row.id, subject: m.subject(), from: m.sender(), ts: Math.floor(row.ms / 1000), date: fmt(when),
            read: m.readStatus(), flagged: m.flaggedStatus()};
  } catch (e) { return null; }
  if (when.getTime() !== row.ms) { throw new Error('mailbox_changed_during_read'); }
  return item;
}

function queryMessages(acc, req) {
  var mb = findMailbox(acc, req.mailbox, '', 3);
  if (!mb) { throw new Error('mailbox_not_found'); }
  var msgs = mb.ref.messages, ids = msgs.id(), dates = msgs.dateReceived(), unread = null, wanted = null, cut = null,
      rows = [], items = [], i, ms, row;
  if (dates.length !== ids.length) { throw new Error('mailbox_changed_during_read'); }
  if (req.query) {
    wanted = searchIds(msgs, req);
  } else {
    if (req.sinceDays) { cut = Date.now() - req.sinceDays * 86400000; }
    if (req.unreadOnly) {
      unread = msgs.readStatus();
      if (unread.length !== ids.length) { throw new Error('mailbox_changed_during_read'); }
    }
  }
  for (i = 0; i < ids.length; i++) {
    ms = dates[i] ? dates[i].getTime() : NaN;
    if (isNaN(ms)) { continue; }
    if (wanted !== null) {
      if (wanted[ids[i]] !== true) { continue; }
    } else if ((cut !== null && !(ms > cut)) || (unread !== null && unread[i] !== false)) {
      continue;
    }
    rows.push({id: ids[i], ms: ms});
  }
  if (rows.length > req.maxHits) { throw new Error('too_many_hits:' + rows.length); }
  rows.sort(function (a, b) { return b.ms - a.ms || b.id - a.id; });
  for (i = 0; i < rows.length && items.length < req.limit; i++) {
    row = listedRow(msgs, rows[i]);
    if (row !== null) { items.push(row); }
  }
  return {mailbox: mb.path, total: rows.length, items: items};
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

function snapshotOfMessage(m) {
  return {subject: String(m.subject()), from: String(m.sender()), ts: Math.floor(m.dateReceived().getTime() / 1000)};
}

function boxRef(acc, path) {
  var parts = path.split('/'), ref = acc, i;
  for (i = 0; i < parts.length; i++) { ref = ref.mailboxes.byName(parts[i]); }
  return ref;
}

function locateMailbox(acc, wanted, depth) {
  var found = findMailbox(acc, wanted, '', depth);
  return found ? {ref: boxRef(acc, found.path), path: found.path} : null;
}

function isGmail(acc) {
  try { return /(^|\.)(gmail|googlemail)\.com$/i.test(String(acc.serverName())); } catch (e) { return false; }
}

function midOf(m) {
  try {
    var mid = m.messageId();
    return mid ? String(mid) : null;
  } catch (e) { return null; }
}

function listing(box) {
  var ids, has = {}, i;
  try { ids = box.ref.messages.id(); } catch (e) { return null; }
  for (i = 0; i < ids.length; i++) { has[ids[i]] = true; }
  ids.sort(function (a, b) { return b - a; });
  return {has: has, newest: ids};
}

function moveItems(acc, box, ids) {
  var items = [], listed = listing(box), i, m, snap;
  if (listed === null) { throw new Error('message_not_found'); }
  for (i = 0; i < ids.length; i++) {
    if (listed.has[ids[i]] !== true) { throw new Error('message_not_found'); }
    m = box.ref.messages.byId(ids[i]);
    if (!ownedBy(m, acc)) { throw new Error('message_not_in_account'); }
    try { snap = snapshotOfMessage(m); } catch (e) { throw new Error('message_not_found'); }
    items.push({id: ids[i], subject: snap.subject, from: snap.from, ts: snap.ts, mid: midOf(m)});
  }
  return items;
}

function moveLines(req, sourcePath, target, items, gmail) {
  var lines = ['Goose m\u00f6chte ' + items.length + (items.length === 1 ? ' E-Mail' : ' E-Mails') + ' verschieben.',
               'Pr\u00fcfe alle Angaben. Nur best\u00e4tigen, wenn du das gerade selbst veranlasst hast.', '',
               row('Konto', req.accountLabel), row('Von', sourcePath),
               row('Nach', target ? target.path : flat(req.target, 120) + '  (wird neu angelegt)')], i;
  if (gmail) { lines.push(rowOf('Hinweis', 'Gmail setzt nur das Etikett, die Mail bleibt zus\u00e4tzlich im Quellpostfach.')); }
  lines.push('', 'E-Mails (Datum, Absender, Betreff):');
  for (i = 0; i < items.length; i++) {
    lines.push(NOTE_MARK + fmt(new Date(items[i].ts * 1000)) + '  ' + flat(items[i].from, 32) + '  ' + flat(items[i].subject, 56));
  }
  return lines;
}

function validMailboxName(name) {
  return name === flat(name, 101) && name !== '' && name.length <= 100 && name.charAt(0) !== '.' &&
         !/[\/\\:\[\]*%"]/.test(name);
}

function createMailbox(acc, name) {
  if (!validMailboxName(name)) { throw new Error('invalid_mailbox_name'); }
  try {
    acc.mailboxes.push(mail().Mailbox({name: name}));
  } catch (e) {
    throw new Error('mailbox_create_failed: ' + String((e && e.message) || e));
  }
  var made = locateMailbox(acc, name, 0);
  if (!made) { throw new Error('mailbox_create_failed'); }
  return made;
}

function countOf(box) {
  try { return box.ref.messages.length; } catch (e) { return -1; }
}

function subCountOf(box) {
  try { return box.ref.mailboxes.length; } catch (e) { return -1; }
}

function newestMids(box, listed, room) {
  var found = {}, i;
  for (i = 0; i < listed.newest.length && i < room; i++) {
    try { found[String(box.ref.messages.byId(listed.newest[i]).messageId())] = true; } catch (e) {}
  }
  return found;
}

function confirmMove(src, target, items) {
  var last = -2, calm = 0, tries, now, room = items.length + 5, quiet = Math.min(6 + 2 * items.length, 60),
      inSource, inTarget, midsSource, midsTarget, i, item, here, there,
      result = {moved: 0, kept: 0, refused: 0, lost: 0};
  for (tries = 0; tries < 120 && calm < quiet; tries++) {
    sleepSeconds(0.5);
    now = countOf(target);
    calm = now === last ? calm + 1 : 0;
    last = now;
  }
  if (calm < quiet) { throw new Error('move_incomplete:' + items.length); }
  inSource = listing(src);
  inTarget = listing(target);
  if (inSource === null || inTarget === null) { throw new Error('move_incomplete:' + items.length); }
  midsSource = newestMids(src, inSource, room);
  midsTarget = newestMids(target, inTarget, room);
  for (i = 0; i < items.length; i++) {
    item = items[i];
    here = inSource.has[item.id] === true;
    there = inTarget.has[item.id] === true || (item.mid !== null && midsTarget[item.mid] === true);
    if (there) {
      result.moved++;
      if (here) { result.kept++; }
    } else if (here || (item.mid !== null && midsSource[item.mid] === true)) {
      result.refused++;
    } else {
      result.lost++;
    }
  }
  return result;
}

function moveMessages(acc, req) {
  var app = mail(), ids = req.ids, i, src, target, items, decision, created = false, moved = 0;
  if (!Array.isArray(ids) || ids.length < 1 || ids.length > req.maxIds) { throw new Error('too_many_messages'); }
  for (i = 0; i < ids.length; i++) {
    if (typeof ids[i] !== 'number' || ids[i] !== Math.floor(ids[i]) || ids[i] < 1) { throw new Error('message_not_found'); }
  }
  src = locateMailbox(acc, req.mailbox, 3);
  if (!src) { throw new Error('mailbox_not_found'); }
  target = locateMailbox(acc, req.target, 3);
  if (!target && !req.create) { throw new Error('target_missing'); }
  if (!target && !validMailboxName(req.target)) { throw new Error('invalid_mailbox_name'); }
  if (target && target.path === src.path) { throw new Error('same_mailbox'); }
  items = moveItems(acc, src, ids);
  decision = showDialog('Goose m\u00f6chte E-Mails verschieben', moveLines(req, src.path, target, items, isGmail(acc)), 'Verschieben', req.seconds);
  if (decision !== 'approved') { return {status: decision}; }
  src = locateMailbox(acc, req.mailbox, 3);
  if (!src) { throw new Error('mailbox_not_found'); }
  if (JSON.stringify(moveItems(acc, src, ids)) !== JSON.stringify(items)) { throw new Error('messages_changed'); }
  target = locateMailbox(acc, req.target, 3);
  if (!target) {
    if (!req.create) { throw new Error('target_missing'); }
    target = createMailbox(acc, req.target);
    created = true;
  }
  if (target.path === src.path) { throw new Error('same_mailbox'); }
  var bare = countOf(target) === 0 && subCountOf(target) === 0;
  try {
    for (i = 0; i < ids.length; i++) {
      app.move(src.ref.messages.byId(ids[i]), {to: target.ref});
      moved++;
    }
  } catch (e) {
    throw new Error('move_state_unknown: ' + moved + ' of ' + ids.length + ': ' + String((e && e.message) || e));
  }
  var after = confirmMove(src, target, items);
  if (after.lost > 0) { throw new Error('move_incomplete:' + after.lost); }
  if (after.moved === 0) {
    throw new Error(created ? 'move_rejected_new_mailbox' : bare ? 'move_rejected_empty' : 'move_rejected');
  }
  return {status: 'moved', source: src.path, target: target.path, created: created, moved: after.moved, kept: after.kept, refused: after.refused};
}
