// The two filters that decide what is worth remembering.
//
// Both were wrong until 2026-07-21 and made the index 25% junk:
//   is_noise            matched "<command-name>" but transcripts store the envelope
//                       as "<command-message>" — all 85 slash-command rows walked in.
//   looks_like_correction  matched bare words including "no", "why", "fix", so
//                       "why are people hyped about shodan" scored as a correction
//                       and the flag carried no information at all.
#pragma once

#include <string>
#include <string_view>

namespace cml {

// Machine-generated turns that wear a human role in the transcript: slash-command
// envelopes, hook injections, task notifications, compaction boilerplate, bare acks.
bool is_noise(std::string_view text);

// A correction is a *phrase*, never a bare word. False negatives cost one unflagged
// line; false positives poison every entry in the inbox.
bool looks_like_correction(std::string_view text);

// Collapse runs of whitespace, then clip to `max` characters (UTF-8 safe at the
// byte level only in the sense that it never splits mid-multibyte).
std::string squeeze(std::string_view s, std::size_t max);

}  // namespace cml
