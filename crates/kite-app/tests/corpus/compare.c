int compare(const char *left, const char *right) {
    if (left == 0 || right == 0) {
        return left == right ? 0 : -1;
    }
    int index = 0;
    while (left[index] != '\0' && right[index] != '\0') {
        if (left[index] < right[index]) {
            return -1;
        }
        if (left[index] > right[index]) {
            return 1;
        }
        index = index + 1;
    }
    if (left[index] == '\0' && right[index] != '\0') {
        return -1;
    }
    if (left[index] != '\0' && right[index] == '\0') {
        return 1;
    }
    return 0;
}
