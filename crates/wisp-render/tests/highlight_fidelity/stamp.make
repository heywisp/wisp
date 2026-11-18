TOOLSET := target
$(obj).target/../node-addon-api/node_addon_api.stamp: TOOLSET := $(TOOLSET)
WGET := $(if $(GITHUB_TOKEN),wget --header="Authorization: $(GITHUB_TOKEN)",wget)
CFLAGS += -O2 -Wall # optimise
all: build test
	@echo "building $@ from $<"
	$(CC) $(CFLAGS) -o $@ $^
.PHONY: all
