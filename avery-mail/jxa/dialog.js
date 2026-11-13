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
  throw new Error('unknown_op');
}
