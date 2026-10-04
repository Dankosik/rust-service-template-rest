threads = {}
function setup(thread)
  table.insert(threads, thread)
end

function init(args)
  index = 0
  n200, n404, n5xx, other = 0, 0, 0, 0
  requests = {
    wrk.format('GET', '/health/live'),
    wrk.format('GET', '/health/ready'),
    wrk.format('GET', '/missing'),
    wrk.format('POST', '/webhooks/unknown', {['content-type']='application/json'}, string.rep('x', 1024)),
  }
end

function request()
  index = (index + 1) % 10
  if index < 4 then return requests[1] end
  if index < 8 then return requests[2] end
  if index == 8 then return requests[3] end
  return requests[4]
end

function response(status, headers, body)
  if status == 200 then n200 = n200 + 1
  elseif status == 404 then n404 = n404 + 1
  elseif status >= 500 then n5xx = n5xx + 1
  else other = other + 1 end
end

function done(summary, latency, requests)
  local ok, expected404, failed, unexpected = 0, 0, 0, 0
  for _, thread in ipairs(threads) do
    ok = ok + thread:get('n200')
    expected404 = expected404 + thread:get('n404')
    failed = failed + thread:get('n5xx')
    unexpected = unexpected + thread:get('other')
  end
  io.write(string.format('WRK_JSON {"requests":%d,"duration_s":%.6f,"rps":%.3f,"p50_ms":%.6f,"p95_ms":%.6f,"p99_ms":%.6f,"status200":%d,"status404":%d,"status5xx":%d,"other":%d,"connect_errors":%d,"read_errors":%d,"write_errors":%d,"timeouts":%d}\n',
    summary.requests, summary.duration / 1e6, summary.requests * 1e6 / summary.duration,
    latency:percentile(50) / 1000, latency:percentile(95) / 1000, latency:percentile(99) / 1000,
    ok, expected404, failed, unexpected, summary.errors.connect, summary.errors.read,
    summary.errors.write, summary.errors.timeout))
end
