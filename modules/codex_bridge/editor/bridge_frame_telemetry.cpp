/**************************************************************************/
/*  bridge_frame_telemetry.cpp                                            */
/**************************************************************************/
/*                         This file is part of:                          */
/*                             GODOT ENGINE                               */
/*                        https://godotengine.org                         */
/**************************************************************************/
/* Copyright (c) 2014-present Godot Engine contributors (see AUTHORS.md). */
/* Copyright (c) 2007-2014 Juan Linietsky, Ariel Manzur.                  */
/*                                                                        */
/* Permission is hereby granted, free of charge, to any person obtaining  */
/* a copy of this software and associated documentation files (the        */
/* "Software"), to deal in the Software without restriction, including    */
/* without limitation the rights to use, copy, modify, merge, publish,    */
/* distribute, sublicense, and/or sell copies of the Software, and to     */
/* permit persons to whom the Software is furnished to do so, subject to  */
/* the following conditions:                                              */
/*                                                                        */
/* The above copyright notice and this permission notice shall be         */
/* included in all copies or substantial portions of the Software.        */
/*                                                                        */
/* THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,        */
/* EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF     */
/* MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. */
/* IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY   */
/* CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT,   */
/* TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE      */
/* SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.                 */
/**************************************************************************/

#include "bridge_frame_telemetry.h"

#include "core/variant/array.h"
#include "core/variant/variant.h"

void BridgeFrameTelemetry::reset(bool p_enabled) {
	enabled = p_enabled;
	overflow = false;
	busy_frame_count = 0;
	over_budget_count = 0;
	max_elapsed_usec = 0;
	samples_usec.clear();
}

void BridgeFrameTelemetry::record(uint64_t p_elapsed_usec, bool p_busy) {
	if (!enabled || !p_busy) {
		return;
	}

	busy_frame_count++;
	max_elapsed_usec = MAX(max_elapsed_usec, p_elapsed_usec);
	if (p_elapsed_usec > BUDGET_USEC) {
		over_budget_count++;
	}
	if ((uint64_t)samples_usec.size() < MAX_SAMPLES) {
		samples_usec.push_back((int64_t)p_elapsed_usec);
	} else {
		overflow = true;
	}
}

bool BridgeFrameTelemetry::is_enabled() const {
	return enabled;
}

Dictionary BridgeFrameTelemetry::to_dictionary() const {
	Array samples;
	samples.resize(samples_usec.size());
	for (int index = 0; index < samples_usec.size(); index++) {
		samples[index] = samples_usec[index];
	}

	Dictionary result;
	result["schema_version"] = 1;
	result["budget_usec"] = (int64_t)BUDGET_USEC;
	result["sample_capacity"] = (int64_t)MAX_SAMPLES;
	result["busy_frame_count"] = (int64_t)busy_frame_count;
	result["samples_usec"] = samples;
	result["max_elapsed_usec"] = (int64_t)max_elapsed_usec;
	result["over_budget_count"] = (int64_t)over_budget_count;
	result["overflow"] = overflow;
	return result;
}
