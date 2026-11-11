// A program linked as other distributions link theirs: it asks for /lib64's loader and has no run
// path, so it only runs where something there finds its libraries.
#include <cstdlib>
#include <iostream>
#include <string>
#include <unistd.h>

int main() {
    char exe[4096] = {0};
    readlink("/proc/self/exe", exe, sizeof exe - 1);
    std::string text = "hello from elsewhere";
    std::cout << text << "\n" << exe << "\n";
    const char *path = std::getenv("LD_LIBRARY_PATH");
    std::cout << "library path: " << (path ? path : "none") << "\n";
    return 0;
}
