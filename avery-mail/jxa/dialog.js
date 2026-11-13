var MAX_TITLE = 80, MAX_TEXT = 2000, MAX_NOTE = 300, MAX_LABEL = 40, MAX_ITEMS = 500, MAX_ITEM = 160;
var MAX_ANSWER = 1000, MAX_SECONDS = 3600, MAX_URL = 300;
var URL_PATTERN = /^https:\/\/[A-Za-z0-9.-]+(\/[A-Za-z0-9._~!$&'()*+,;=:@%\/#?-]*)?$/;

function run(argv) {
  var req = JSON.parse(argv[0]);
  try {
    return JSON.stringify({ok: true, data: dispatch(req)});
  } catch (e) {
    return JSON.stringify({ok: false, error: String((e && e.message) || e)});
  }
}

function dispatch(req) {
  if (req.op === 'ping') { return {jxa: true}; }
  if (req.op === 'approve_send') { return {decision: askApproval(req, req.seconds)}; }
  if (req.op === 'approve_change') { return {decision: changeDecision(req, req.before, req.after)}; }
  if (req.op === 'ask_text') { return askText(req); }
  if (req.op === 'ask_buttons') { return askButtons(req); }
  if (req.op === 'choose_list') { return chooseList(req); }
  if (req.op === 'notify') { return notify(req); }
  if (req.op === 'open_url') { return openUrl(req); }
  throw new Error('unknown_op');
}

function bad(what) { throw new Error('bad_request: ' + what); }

function field(value, max, what) {
  if (typeof value !== 'string') { bad(what); }
  var text = shown(value).replace(/\r\n?/g, '\n');
  if (Array.from(text).length > max) { bad(what); }
  return text;
}

function label(value, what) {
  var text = field(value, MAX_LABEL, what).replace(/\s+/g, ' ').trim();
  if (text === '') { bad(what); }
  return text;
}

function whole(value, low, high, what) {
  if (typeof value !== 'number' || value !== Math.floor(value) || value < low || value > high) { bad(what); }
  return value;
}

function indexes(value, size, what) {
  if (!Array.isArray(value) || value.length > size) { bad(what); }
  var seen = [];
  for (var i = 0; i < value.length; i++) {
    whole(value[i], 0, size - 1, what);
    if (seen.indexOf(value[i]) >= 0) { bad(what); }
    seen.push(value[i]);
  }
  return seen;
}

function screen(front) {
  var app = Application.currentApplication();
  app.includeStandardAdditions = true;
  if (front) { try { app.activate(); } catch (e) {} }
  return app;
}

function declined(e) { return String((e && e.message) || e).indexOf('-128') >= 0; }

function broken(e) { return new Error('dialog_failed: ' + String((e && e.message) || e)); }

function icon(value) {
  if (value === undefined || value === 'note') { return 'note'; }
  if (value === 'caution' || value === 'stop') { return value; }
  return bad('icon');
}

function askText(req) {
  var title = field(req.title, MAX_TITLE, 'title'), text = field(req.text, MAX_TEXT, 'text');
  var initial = field(req['default'], MAX_ANSWER, 'default'), ok = label(req.ok, 'ok'), cancel = label(req.cancel, 'cancel');
  var max = whole(req.max, 1, MAX_ANSWER, 'max'), seconds = whole(req.seconds, 1, MAX_SECONDS, 'seconds');
  if (typeof req.hidden !== 'boolean') { bad('hidden'); }
  if (ok === cancel) { bad('ok'); }
  var look = icon(req.icon);
  var app = screen(true), answer;
  try {
    answer = app.displayDialog(text, {
      withTitle: title, defaultAnswer: initial, hiddenAnswer: req.hidden, buttons: [cancel, ok],
      defaultButton: ok, cancelButton: cancel, withIcon: look, givingUpAfter: seconds
    });
  } catch (e) {
    if (declined(e)) { return {result: 'cancel'}; }
    throw broken(e);
  }
  if (answer.gaveUp) { return {result: 'timeout'}; }
  if (answer.buttonReturned !== ok) { return {result: 'cancel'}; }
  var typed = String(answer.textReturned);
  if (Array.from(typed).length > max) { return {result: 'too_long'}; }
  return {result: 'text', text: typed};
}

function askButtons(req) {
  var title = field(req.title, MAX_TITLE, 'title'), text = field(req.text, MAX_TEXT, 'text');
  if (!Array.isArray(req.buttons) || req.buttons.length < 1 || req.buttons.length > 3) { bad('buttons'); }
  var names = [];
  for (var i = 0; i < req.buttons.length; i++) {
    names.push(label(req.buttons[i], 'buttons'));
    if (names.indexOf(names[i]) !== i) { bad('buttons'); }
  }
  var first = whole(req['default'], 0, names.length - 1, 'default');
  var cancel = req.cancel === null ? null : whole(req.cancel, 0, names.length - 1, 'cancel');
  var seconds = whole(req.seconds, 1, MAX_SECONDS, 'seconds');
  var options = {withTitle: title, buttons: names, defaultButton: names[first], withIcon: icon(req.icon), givingUpAfter: seconds};
  if (cancel !== null) { options.cancelButton = names[cancel]; }
  var app = screen(true), answer;
  try {
    answer = app.displayDialog(text, options);
  } catch (e) {
    if (declined(e)) { return {result: 'cancel'}; }
    throw broken(e);
  }
  if (answer.gaveUp) { return {result: 'timeout'}; }
  var index = names.indexOf(answer.buttonReturned);
  if (index < 0) { throw new Error('dialog_failed: unknown button'); }
  return {result: 'button', index: index};
}

function chooseList(req) {
  var title = field(req.title, MAX_TITLE, 'title'), text = field(req.text, MAX_TEXT, 'text');
  if (!Array.isArray(req.items) || req.items.length < 1 || req.items.length > MAX_ITEMS) { bad('items'); }
  var items = [];
  for (var i = 0; i < req.items.length; i++) {
    items.push(field(req.items[i], MAX_ITEM, 'items').replace(/\s+/g, ' ').trim());
    if (items[i] === '' || items.indexOf(items[i]) !== i) { bad('items'); }
  }
  if (typeof req.multiple !== 'boolean') { bad('multiple'); }
  var firsts = indexes(req.defaults, items.length, 'defaults');
  if (!req.multiple && firsts.length > 1) { bad('defaults'); }
  var ok = label(req.ok, 'ok'), cancel = label(req.cancel, 'cancel');
  whole(req.seconds, 1, MAX_SECONDS, 'seconds');
  var preset = [];
  for (var j = 0; j < firsts.length; j++) { preset.push(items[firsts[j]]); }
  var app = screen(true), chosen;
  try {
    chosen = app.chooseFromList(items, {
      withTitle: title, withPrompt: text, defaultItems: preset, okButtonName: ok, cancelButtonName: cancel,
      multipleSelectionsAllowed: req.multiple, emptySelectionAllowed: true
    });
  } catch (e) {
    if (declined(e)) { return {result: 'cancel'}; }
    throw broken(e);
  }
  if (chosen === false) { return {result: 'cancel'}; }
  if (!Array.isArray(chosen) || chosen.length > items.length) { throw new Error('dialog_failed: unexpected selection'); }
  var selected = [];
  for (var k = 0; k < chosen.length; k++) {
    var at = items.indexOf(String(chosen[k]));
    if (at < 0 || selected.indexOf(at) >= 0) { throw new Error('dialog_failed: unknown item'); }
    selected.push(at);
  }
  selected.sort(function (a, b) { return a - b; });
  return {result: 'list', selected: selected};
}

function notify(req) {
  var title = field(req.title, MAX_TITLE, 'title'), text = field(req.text, MAX_NOTE, 'text');
  try {
    screen(false).displayNotification(text, {withTitle: title});
  } catch (e) {
    throw broken(e);
  }
  return {result: 'ok'};
}

function openUrl(req) {
  if (typeof req.url !== 'string' || req.url.length > MAX_URL || !URL_PATTERN.test(req.url)) { bad('url'); }
  try {
    screen(false).openLocation(req.url);
  } catch (e) {
    throw broken(e);
  }
  return {result: 'ok'};
}
