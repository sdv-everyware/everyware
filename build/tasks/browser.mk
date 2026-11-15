#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#      http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.
#

# The browser's Debian root (browser/apex): an image with EVERYWARE_EXPERIMENTAL_BROWSER=true
# carries it, and its image of the architecture is not in git. Without it the build stops here,
# with what to do, instead of building an APEX with an empty root.
ifeq ($(SOONG_CONFIG_everyware_experimental_browser),true)
EVERYWARE_BROWSER_ARCH := $(patsubst x86_64,amd64,$(filter arm64 x86_64,$(TARGET_ARCH)))
ifeq ($(EVERYWARE_BROWSER_ARCH),)
$(error EVERYWARE_EXPERIMENTAL_BROWSER: no Debian root for $(TARGET_ARCH), only for arm64 and x86_64)
endif
ifeq ($(wildcard vendor/sdv-everyware/everyware/browser/apex/prebuilt/$(EVERYWARE_BROWSER_ARCH)/rootfs.erofs),)
$(error EVERYWARE_EXPERIMENTAL_BROWSER: no Debian root in vendor/sdv-everyware/everyware/browser/apex/prebuilt/$(EVERYWARE_BROWSER_ARCH): make it on an $(EVERYWARE_BROWSER_ARCH) board with vendor/sdv-everyware/everyware/scripts/browser-root.sh -s <serial> --export (docs/project/browser.md))
endif
endif
