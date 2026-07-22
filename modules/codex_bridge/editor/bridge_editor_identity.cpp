/**************************************************************************/
/*  bridge_editor_identity.cpp                                           */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/**************************************************************************/

#include "bridge_editor_identity.h"

#include "core/crypto/crypto_core.h"
#include "scene/main/node.h"

#include "modules/codex_bridge/protocol/bridge_crypto.h"

String BridgeEditorIdentity::make_opaque_id(const String &p_prefix, const String &p_domain, const String &p_value) {
	const CharString bytes = (p_domain + "\n" + p_value).utf8();
	PackedByteArray digest;
	digest.resize(32);
	if (CryptoCore::sha256(reinterpret_cast<const uint8_t *>(bytes.get_data()), bytes.length(), digest.ptrw()) != OK) {
		return p_prefix + "00000000000000000000000000000000";
	}
	return p_prefix + BridgeCrypto::bytes_to_lower_hex(digest).left(32);
}

String BridgeEditorIdentity::make_scene_id(const String &p_editor_session_id, const Node *p_scene_root) {
	if (!p_scene_root) {
		return String();
	}
	String identity = p_scene_root->get_scene_file_path();
	if (identity.is_empty()) {
		identity = String(p_scene_root->get_name()) + "\n" + String::num_uint64(p_scene_root->get_instance_id());
	}
	return make_opaque_id("scene:", "godot-codex-scene/v1\n" + p_editor_session_id, identity);
}

String BridgeEditorIdentity::make_node_id(const String &p_editor_session_id, const String &p_scene_id, const String &p_node_path) {
	return make_opaque_id("node:", "godot-codex-node/v1\n" + p_editor_session_id + "\n" + p_scene_id, p_node_path);
}

String BridgeEditorIdentity::make_history_id(const String &p_editor_session_id, int p_native_history_id) {
	return make_opaque_id("history:", "godot-codex-history/v1\n" + p_editor_session_id, String::num_int64(p_native_history_id));
}

String BridgeEditorIdentity::make_script_id(const String &p_editor_session_id, const String &p_identity) {
	return make_opaque_id("script:", "godot-codex-live-script/v1\n" + p_editor_session_id, p_identity);
}
