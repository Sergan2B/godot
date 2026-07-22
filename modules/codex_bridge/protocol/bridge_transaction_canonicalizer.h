/**************************************************************************/
/*  bridge_transaction_canonicalizer.h                                   */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/**************************************************************************/

#pragma once

#include "core/variant/variant.h"

class BridgeTransactionCanonicalizer {
public:
	static Dictionary normalize_operation(const Dictionary &p_operation);
	static Error make_request_digest(const String &p_project_id, const String &p_editor_session_id, const Dictionary &p_prepare_params, String &r_canonical_json, String &r_digest);
	static Error make_preview_digest(const Dictionary &p_payload, String &r_canonical_json, String &r_digest);
	static Error sha256_utf8(const String &p_value, String &r_digest, const String &p_domain = String());
};
