CXX      ?= g++
CXXFLAGS ?= -O2 -std=c++17 -Wall -Wextra

halfpot: bot.cpp
	$(CXX) $(CXXFLAGS) -o $@ $<

clean:
	rm -f halfpot

.PHONY: clean
