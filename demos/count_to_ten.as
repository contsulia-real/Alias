func i32 main = () -> {
    var i32 i = 0;

    func bool cond = (i32 current, i32 limit) -> return current < limit

    while cond(i, 10) {
        increase i

        println i
    }

    return 0
}
