#!/bin/bash
# Tear down the test stand container. .gen/ (disposable keys) is kept;
# delete it manually for a fully fresh stand.
docker rm -f "${AWG_TEST_CONTAINER:-amnezia-legacy-test}" >/dev/null 2>&1 || true
echo "container removed"
