include versions.env

IMAGE ?= ghcr.io/agentteams-group/agentteams-harness-openhuman
VERSION ?= v0.1.0
PLATFORMS ?= linux/amd64,linux/arm64
AT_SOURCE ?= ../AgentTeams
AT_CONTEXT ?= https://github.com/agentscope-ai/AgentTeams.git\#$(AT_REF)
BUILD_ARGS = --build-context agentteams=$(AT_CONTEXT) --build-arg OPENHUMAN_GIT_REF=$(OPENHUMAN_GIT_REF) --build-arg AGENTTEAMS_CONTROLLER_IMAGE=$(AGENTTEAMS_CONTROLLER_IMAGE)

.PHONY: build push test smoke
build:
	docker build $(BUILD_ARGS) $(DOCKER_BUILD_ARGS) -t $(IMAGE):$(VERSION) .
push:
	docker buildx build --platform $(PLATFORMS) $(BUILD_ARGS) $(DOCKER_BUILD_ARGS) -t $(IMAGE):$(VERSION) --push .
test:
	AT_SOURCE=$(abspath $(AT_SOURCE)) python3 -m pytest tests -q
	cargo test --locked --manifest-path tests/session-receipts/Cargo.toml
	bash -n scripts/openhuman-worker-entrypoint.sh scripts/agt
smoke:
	docker run --rm --entrypoint openhuman-core $(IMAGE):$(VERSION) --help
	docker run --rm --entrypoint sh $(IMAGE):$(VERSION) -ec 'command -v agt; test -r /opt/agentteams/scripts/lib/task_coordination.py; test -r /opt/agentteams/scripts/lib/task_recovery_rpc.py; test -r /opt/agentteams/plugins/teamharness/mcp/server.py; python3 -c "import yaml"; test -s /usr/share/licenses/openhuman/LICENSE'
	docker run --rm --network none --entrypoint python3 -v "$(CURDIR)/tests/native_startup_config_smoke.py:/tmp/native_startup_config_smoke.py:ro" $(IMAGE):$(VERSION) /tmp/native_startup_config_smoke.py
