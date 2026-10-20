// cpp_halfpot - a C++ example bot for the Agentic Poker Arena.
//
// Strategy, in full: wager half the pot, every single time.
//
// Ambiguities and the assumptions made about them:
//
//   * "half the pot" is half of `pot` as the engine reports it, which is every
//     chip committed this hand including the current street.
//   * The target is clamped into the legal window [min_raise_to, max_raise_to].
//     So a half-pot target below the minimum becomes a minimum-sized wager, and
//     one above our stack becomes an all-in. The engine would clamp an oversized
//     raise for us (a Tier 1 normalisation), but a bot that sends a legal action
//     in the first place is the better example.
//   * With no bet outstanding the verb is `bet`; facing a bet it is `raise`.
//     These are not interchangeable: AGENT_SPEC.md §5.2 makes `bet` while a bet
//     is outstanding a Tier 2 violation.
//   * When raising is impossible (`min_raise_to == 0`, i.e. we cannot even cover
//     the outstanding bet) it calls, which the engine turns into an all-in call.
//     Folding would also be defensible; calling keeps this bot maximally
//     aggressive, which is what makes it a useful sparring partner.
//
// JSON handling: this reads the few scalar fields it needs with a small
// key scanner rather than a real parser. That is sound here only because every
// field it wants -- type, id, pot, current_bet, min_raise_to, max_raise_to --
// appears exactly once in an engine message and never nested inside the `seats`
// or `hand_log` arrays. A bot that needed `seat`, `street` or `amount` would
// have to parse properly, since those names also occur inside those arrays.
//
// Build: make      (or: g++ -O2 -std=c++17 -o halfpot bot.cpp)

#include <cctype>
#include <cstdint>
#include <iostream>
#include <optional>
#include <string>

namespace {

constexpr int PROTOCOL_VERSION = 1;

// Find `"key":` at a position that is not part of a longer key, and return the
// offset just past the colon.
std::optional<size_t> find_key(const std::string& s, const std::string& key) {
    const std::string pattern = "\"" + key + "\":";
    const size_t at = s.find(pattern);
    if (at == std::string::npos) return std::nullopt;
    return at + pattern.size();
}

std::optional<long long> get_int(const std::string& s, const std::string& key) {
    auto start = find_key(s, key);
    if (!start) return std::nullopt;
    size_t i = *start;
    while (i < s.size() && (s[i] == ' ' || s[i] == '\t')) ++i;
    bool negative = false;
    if (i < s.size() && s[i] == '-') { negative = true; ++i; }
    if (i >= s.size() || !std::isdigit(static_cast<unsigned char>(s[i]))) return std::nullopt;
    long long value = 0;
    while (i < s.size() && std::isdigit(static_cast<unsigned char>(s[i]))) {
        value = value * 10 + (s[i] - '0');
        ++i;
    }
    return negative ? -value : value;
}

std::optional<std::string> get_string(const std::string& s, const std::string& key) {
    auto start = find_key(s, key);
    if (!start) return std::nullopt;
    size_t i = *start;
    while (i < s.size() && (s[i] == ' ' || s[i] == '\t')) ++i;
    if (i >= s.size() || s[i] != '"') return std::nullopt;
    ++i;
    std::string out;
    while (i < s.size() && s[i] != '"') {
        if (s[i] == '\\' && i + 1 < s.size()) ++i;  // no escapes in engine output, but be safe
        out.push_back(s[i++]);
    }
    return out;
}

// One JSON object per line, flushed immediately. A buffered line looks exactly
// like a bot that never answered.
void send(const std::string& body) {
    std::cout << "{\"v\":" << PROTOCOL_VERSION << "," << body << "}\n" << std::flush;
}

long long clamp(long long v, long long lo, long long hi) {
    if (v < lo) return lo;
    if (v > hi) return hi;
    return v;
}

void handle_action_request(const std::string& line) {
    const long long id           = get_int(line, "id").value_or(0);
    const long long pot          = get_int(line, "pot").value_or(0);
    const long long current_bet  = get_int(line, "current_bet").value_or(0);
    const long long min_raise_to = get_int(line, "min_raise_to").value_or(0);
    const long long max_raise_to = get_int(line, "max_raise_to").value_or(0);

    const std::string head = "\"type\":\"action\",\"id\":" + std::to_string(id);

    // Cannot raise at all: we cannot even cover what is outstanding. Call, and
    // let the engine turn it into an all-in call.
    if (min_raise_to <= 0) {
        send(head + ",\"action\":\"call\"");
        return;
    }

    const long long target = clamp(pot / 2, min_raise_to, max_raise_to);
    const char* verb = (current_bet == 0) ? "bet" : "raise";
    send(head + ",\"action\":\"" + verb + "\",\"amount\":" + std::to_string(target));
}

}  // namespace

int main() {
    std::ios::sync_with_stdio(false);
    std::string line;

    while (std::getline(std::cin, line)) {
        if (line.empty()) continue;

        const std::string type = get_string(line, "type").value_or("");

        if (type == "entry") {
            const long long id = get_int(line, "id").value_or(0);
            std::cerr << "cpp_halfpot: seat " << get_int(line, "me").value_or(-1)
                      << ", stack " << get_int(line, "starting_stack").value_or(0) << "\n";
            send("\"type\":\"ready\",\"id\":" + std::to_string(id) +
                 ",\"name\":\"cpp_halfpot\",\"author\":\"example\",\"language\":\"cpp\"");
        } else if (type == "action_request") {
            handle_action_request(line);
        } else if (type == "shutdown") {
            std::cerr << "cpp_halfpot: shutting down\n";
            break;
        }
        // hand_start and hand_end expect NO reply. Unknown types are ignored.
    }
    return 0;
}
