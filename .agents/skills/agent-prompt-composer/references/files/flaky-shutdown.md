hay un flake raro, maybe around shutdown / drain, not sure.
Sometimes the cancellation gets swallowed or the background task does not stop and then the test hangs.
No quiero simplemente subir el timeout. Don't just increase timeouts.
First figure out where it breaks, probably bootstrap or the health/readiness thing, then fix it carefully.
Also check the task-lifetime / shutdown-stage angle if that is the right proof path.
