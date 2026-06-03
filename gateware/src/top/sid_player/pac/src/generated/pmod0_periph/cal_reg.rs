#[doc = "Register `cal_reg` reader"]
pub type R = crate::R<CAL_REG_SPEC>;
#[doc = "Register `cal_reg` writer"]
pub type W = crate::W<CAL_REG_SPEC>;
#[doc = "Field `channel` writer - channel field"]
pub type CHANNEL_W<'a, REG> = crate::FieldWriter<'a, REG, 3>;
#[doc = "Field `write` writer - write field"]
pub type WRITE_W<'a, REG> = crate::BitWriter<'a, REG>;
#[doc = "Field `done` reader - done field"]
pub type DONE_R = crate::BitReader;
impl R {
    #[doc = "Bit 4 - done field"]
    #[inline(always)]
    pub fn done(&self) -> DONE_R {
        DONE_R::new(((self.bits >> 4) & 1) != 0)
    }
}
impl W {
    #[doc = "Bits 0:2 - channel field"]
    #[inline(always)]
    pub fn channel(&mut self) -> CHANNEL_W<'_, CAL_REG_SPEC> {
        CHANNEL_W::new(self, 0)
    }
    #[doc = "Bit 3 - write field"]
    #[inline(always)]
    pub fn write(&mut self) -> WRITE_W<'_, CAL_REG_SPEC> {
        WRITE_W::new(self, 3)
    }
}
#[doc = "A CSR register. Parameters ---------- fields : :class:`dict` or :class:`list` or :class:`Field` Collection of register fields. If ``None`` (default), a dict is populated from Python :term:`variable annotations <python:variable annotations>`. ``fields`` is used to create a :class:`FieldActionMap`, :class:`FieldActionArray`, or :class:`FieldAction`, depending on its type (dict, list, or Field). Interface attributes -------------------- element : :class:`Element` Interface between this register and a CSR bus primitive. Attributes ---------- field : :class:`FieldActionMap` or :class:`FieldActionArray` or :class:`FieldAction` Collection of field instances. f : :class:`FieldActionMap` or :class:`FieldActionArray` or :class:`FieldAction` Shorthand for :attr:`Register.field`. Raises ------ :exc:`TypeError` If ``fields`` is neither ``None``, a :class:`dict`, a :class:`list`, or a :class:`Field`. :exc:`ValueError` If ``fields`` is not ``None`` and at least one variable annotation is a :class:`Field`. :exc:`ValueError` If ``element.access`` is not readable and at least one field is readable. :exc:`ValueError` If ``element.access`` is not writable and at least one field is writable.\n\nYou can [`read`](crate::Reg::read) this register and get [`cal_reg::R`](R). You can [`reset`](crate::Reg::reset), [`write`](crate::Reg::write), [`write_with_zero`](crate::Reg::write_with_zero) this register using [`cal_reg::W`](W). You can also [`modify`](crate::Reg::modify) this register. See [API](https://docs.rs/svd2rust/#read--modify--write-api)."]
pub struct CAL_REG_SPEC;
impl crate::RegisterSpec for CAL_REG_SPEC {
    type Ux = u8;
}
#[doc = "`read()` method returns [`cal_reg::R`](R) reader structure"]
impl crate::Readable for CAL_REG_SPEC {}
#[doc = "`write(|w| ..)` method takes [`cal_reg::W`](W) writer structure"]
impl crate::Writable for CAL_REG_SPEC {
    type Safety = crate::Unsafe;
}
#[doc = "`reset()` method sets cal_reg to value 0"]
impl crate::Resettable for CAL_REG_SPEC {}
