import { string.moreRespectful } from './helper.as'

func i32 main = () -> {
    var i32 i = 0;

    func bool cond = (i32 x) -> return i < x

    for cond(10) {
        increase i

        println i
    }

    return 0
}